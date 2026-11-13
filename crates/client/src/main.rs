//! Клиент для Windows: принимает H.264 от remote-host, декодирует, показывает
//! в окне и отправляет мышь и клавиатуру обратно.
//!
//! Задержка держится малой так: декодер работает в отдельном потоке, в окно
//! попадает только самый свежий кадр (старые отбрасываются), vsync выключен,
//! события мыши склеиваются (не больше одного движения за цикл).

mod keymap;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use proto::{read_message, write_message, Button, Message, VERSION};
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Mod, Scancode};
use sdl2::mouse::MouseButton;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::Texture;
use sdl2::video::FullscreenType;
use std::collections::HashSet;
use std::error::Error;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "\
remote-client — клиент удалённого управления (Windows)

Использование:
  remote-client <адрес[:порт]> --token ТОКЕН [параметры]

Параметры:
  --token TEXT     токен, который напечатал remote-host (или переменная REMOTE_TOKEN)
  --ctrl-as-cmd    Ctrl работает как Cmd (Ctrl+C -> Cmd+C), клавиша Win — как Control
  --fullscreen     запуск на весь экран
  --hide-cursor    скрыть локальный курсор (виден только курсор из видео)

Горячие клавиши клиента (на Mac не передаются):
  Ctrl+Alt+G       захват/освобождение мыши и системных сочетаний (Win, Alt+Tab)
  Ctrl+Alt+F       полноэкранный режим
";

struct Args {
    addr: String,
    token: String,
    ctrl_as_cmd: bool,
    fullscreen: bool,
    hide_cursor: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut addr: Option<String> = None;
    let mut token = std::env::var("REMOTE_TOKEN").ok();
    let mut ctrl_as_cmd = false;
    let mut fullscreen = false;
    let mut hide_cursor = false;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--token" => token = Some(it.next().ok_or("--token требует значение")?),
            "--ctrl-as-cmd" => ctrl_as_cmd = true,
            "--fullscreen" => fullscreen = true,
            "--hide-cursor" => hide_cursor = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with("--") => return Err(format!("неизвестный параметр {s}\n\n{USAGE}")),
            s => addr = Some(s.to_string()),
        }
    }
    let mut addr = addr.ok_or_else(|| format!("не указан адрес хоста\n\n{USAGE}"))?;
    if !addr.contains(':') {
        addr.push_str(":7878");
    }
    let token = token.ok_or_else(|| format!("не указан токен\n\n{USAGE}"))?;
    Ok(Args {
        addr,
        token,
        ctrl_as_cmd,
        fullscreen,
        hide_cursor,
    })
}

/// Один декодированный кадр в формате планарного YUV 4:2:0.
struct Frame {
    w: u32,
    h: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    ys: usize,
    us: usize,
    vs: usize,
}

struct Shared {
    /// самый свежий кадр; старый, не успевший показаться, просто перезаписывается
    latest: Mutex<Option<Frame>>,
    decoded: AtomicU64,
    rtt_us: AtomicU64,
    alive: AtomicBool,
}

/// Копия плоскости с гарантированной длиной stride*rows (SDL проверяет размер).
fn plane(src: &[u8], stride: usize, rows: usize) -> Vec<u8> {
    let mut v = vec![0u8; stride * rows];
    let n = src.len().min(v.len());
    v[..n].copy_from_slice(&src[..n]);
    v
}

fn micros_since(t0: Instant) -> u64 {
    t0.elapsed().as_micros() as u64
}

/// Поток приёма: читает сообщения, декодирует видео, считает RTT.
fn reader(mut net: TcpStream, shared: Arc<Shared>, t0: Instant) {
    let mut decoder = match Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не удалось создать декодер H.264: {e}");
            shared.alive.store(false, Ordering::SeqCst);
            return;
        }
    };
    loop {
        match read_message(&mut net) {
            Ok(Message::Video(data)) => match decoder.decode(&data) {
                Ok(Some(yuv)) => {
                    let (w, h) = yuv.dimensions();
                    let (ys, us, vs) = yuv.strides();
                    let ch = (h + 1) / 2;
                    let frame = Frame {
                        w: w as u32,
                        h: h as u32,
                        y: plane(yuv.y(), ys, h),
                        u: plane(yuv.u(), us, ch),
                        v: plane(yuv.v(), vs, ch),
                        ys,
                        us,
                        vs,
                    };
                    *shared.latest.lock().unwrap() = Some(frame);
                    shared.decoded.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => {}
                Err(e) => eprintln!("ошибка декодера (кадр пропущен): {e}"),
            },
            Ok(Message::Pong(v)) => {
                let now = micros_since(t0);
                shared.rtt_us.store(now.saturating_sub(v), Ordering::Relaxed);
            }
            Ok(Message::Error(text)) => {
                eprintln!("хост сообщил об ошибке: {text}");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("соединение закрыто: {e}");
                break;
            }
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
}

fn connect(args: &Args) -> Result<TcpStream, Box<dyn Error>> {
    let mut s = TcpStream::connect(&args.addr)
        .map_err(|e| format!("не удалось подключиться к {}: {e}", args.addr))?;
    s.set_nodelay(true)?;
    write_message(
        &mut s,
        &Message::Hello {
            version: VERSION,
            token: args.token.as_bytes().to_vec(),
        },
    )?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    match read_message(&mut s)? {
        Message::Welcome => {}
        Message::Error(text) => return Err(format!("хост отклонил подключение: {text}").into()),
        other => return Err(format!("неожиданный ответ хоста: {other:?}").into()),
    }
    s.set_read_timeout(None)?;
    Ok(s)
}

/// Где в окне лежит видео: (x, y, ширина, высота) с сохранением пропорций.
fn video_rect(win_w: f32, win_h: f32, vw: f32, vh: f32) -> (f32, f32, f32, f32) {
    let scale = (win_w / vw).min(win_h / vh);
    let (dw, dh) = (vw * scale, vh * scale);
    ((win_w - dw) / 2.0, (win_h - dh) / 2.0, dw, dh)
}

struct Sender {
    net: TcpStream,
    alive: Arc<Shared>,
}

impl Sender {
    fn send(&mut self, msg: &Message) {
        if write_message(&mut self.net, msg).is_err() {
            self.alive.alive.store(false, Ordering::SeqCst);
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    eprintln!("подключаюсь к {} ...", args.addr);
    let net = connect(&args)?;
    eprintln!("подключено");

    let t0 = Instant::now();
    let shared = Arc::new(Shared {
        latest: Mutex::new(None),
        decoded: AtomicU64::new(0),
        rtt_us: AtomicU64::new(0),
        alive: AtomicBool::new(true),
    });
    {
        let rd = net.try_clone()?;
        let sh = Arc::clone(&shared);
        std::thread::spawn(move || reader(rd, sh, t0));
    }
    let mut tx = Sender {
        net,
        alive: Arc::clone(&shared),
    };

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "linear");
    // при захвате ввода (Ctrl+Alt+G) окно получает и системные сочетания клавиш
    sdl2::hint::set("SDL_GRAB_KEYBOARD", "1");

    let mut window = video
        .window("Remote Mac", 1280, 720)
        .position_centered()
        .resizable()
        .build()?;
    if args.fullscreen {
        window.set_fullscreen(FullscreenType::Desktop)?;
    }
    let mut canvas = window.into_canvas().accelerated().build()?; // без vsync: минимум задержки
    let tc = canvas.texture_creator();
    let mut events = sdl.event_pump()?;
    sdl.mouse().show_cursor(!args.hide_cursor);

    let mut texture: Option<Texture> = None;
    let (mut vw, mut vh) = (0u32, 0u32);
    let mut redraw = false;
    let mut grabbed = false;
    let mut pending_move: Option<(i32, i32)> = None;
    let mut held: HashSet<u16> = HashSet::new();
    let mut last_ping = Instant::now();
    let mut last_stats = Instant::now();
    let mut last_decoded = 0u64;
    let mut shown = 0u64;
    let mut last_shown = 0u64;

    'main: while shared.alive.load(Ordering::SeqCst) {
        // ждём события не дольше 1 мс, чтобы быстро подхватывать новые кадры
        let mut batch: Vec<Event> = events.wait_event_timeout(1).into_iter().collect();
        batch.extend(events.poll_iter());

        for ev in batch {
            match ev {
                Event::Quit { .. } => break 'main,
                Event::Window { win_event, .. } => match win_event {
                    WindowEvent::FocusLost => {
                        // отпускаем всё зажатое, чтобы клавиши не «залипали» на Mac
                        for code in held.drain() {
                            tx.send(&Message::Key { code, down: false });
                        }
                    }
                    WindowEvent::Exposed
                    | WindowEvent::Resized(..)
                    | WindowEvent::SizeChanged(..)
                    | WindowEvent::Restored => redraw = true,
                    _ => {}
                },
                Event::MouseMotion { x, y, .. } => pending_move = Some((x, y)),
                Event::MouseButtonDown { mouse_btn, x, y, .. }
                | Event::MouseButtonUp { mouse_btn, x, y, .. } => {
                    let down = matches!(ev, Event::MouseButtonDown { .. });
                    let button = match mouse_btn {
                        MouseButton::Left => Some(Button::Left),
                        MouseButton::Right => Some(Button::Right),
                        MouseButton::Middle => Some(Button::Middle),
                        _ => None,
                    };
                    if let Some(button) = button {
                        // кнопка должна прийти в той точке, где произошёл клик
                        pending_move = Some((x, y));
                        flush_move(&mut tx, &mut pending_move, &canvas, vw, vh);
                        tx.send(&Message::MouseButton { button, down });
                    }
                }
                Event::MouseWheel { x, y, .. } => {
                    tx.send(&Message::Scroll {
                        dx: (x * 40).clamp(-3000, 3000) as i16,
                        dy: (y * 40).clamp(-3000, 3000) as i16,
                    });
                }
                Event::KeyDown {
                    scancode: Some(sc),
                    keymod,
                    repeat,
                    ..
                } => {
                    let ctrl_alt = keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD)
                        && keymod.intersects(Mod::LALTMOD | Mod::RALTMOD);
                    if ctrl_alt && sc == Scancode::G {
                        if !repeat {
                            grabbed = !grabbed;
                            canvas.window_mut().set_grab(grabbed);
                            eprintln!(
                                "захват ввода: {}",
                                if grabbed { "включён" } else { "выключен" }
                            );
                        }
                    } else if ctrl_alt && sc == Scancode::F {
                        if !repeat {
                            let w = canvas.window_mut();
                            let target = if w.fullscreen_state() == FullscreenType::Off {
                                FullscreenType::Desktop
                            } else {
                                FullscreenType::Off
                            };
                            let _ = w.set_fullscreen(target);
                        }
                    } else if let Some(code) = keymap::to_mac(sc, args.ctrl_as_cmd) {
                        held.insert(code);
                        tx.send(&Message::Key { code, down: true });
                    }
                }
                Event::KeyUp {
                    scancode: Some(sc), ..
                } => {
                    if let Some(code) = keymap::to_mac(sc, args.ctrl_as_cmd) {
                        if held.remove(&code) {
                            tx.send(&Message::Key { code, down: false });
                        }
                    }
                }
                _ => {}
            }
        }
        flush_move(&mut tx, &mut pending_move, &canvas, vw, vh);

        // новый кадр: заливаем в YUV-текстуру
        let frame = shared.latest.lock().unwrap().take();
        if let Some(f) = frame {
            if texture.is_none() || vw != f.w || vh != f.h {
                vw = f.w;
                vh = f.h;
                texture = Some(tc.create_texture_streaming(PixelFormatEnum::IYUV, f.w, f.h)?);
            }
            if let Some(t) = texture.as_mut() {
                t.update_yuv(None, &f.y, f.ys, &f.u, f.us, &f.v, f.vs)?;
            }
            redraw = true;
            shown += 1;
        }

        if redraw {
            if let Some(t) = texture.as_ref() {
                let (ow, oh) = canvas.output_size()?;
                let (x, y, w, h) = video_rect(ow as f32, oh as f32, vw as f32, vh as f32);
                canvas.set_draw_color(Color::RGB(0, 0, 0));
                canvas.clear();
                canvas.copy(
                    t,
                    None,
                    Some(Rect::new(x as i32, y as i32, w.max(1.0) as u32, h.max(1.0) as u32)),
                )?;
                canvas.present();
            }
            redraw = false;
        }

        // раз в секунду: ping и статистика в заголовке окна
        if last_ping.elapsed() >= Duration::from_secs(1) {
            tx.send(&Message::Ping(micros_since(t0)));
            last_ping = Instant::now();
        }
        if last_stats.elapsed() >= Duration::from_secs(1) {
            let secs = last_stats.elapsed().as_secs_f32();
            let decoded = shared.decoded.load(Ordering::Relaxed);
            let rtt = shared.rtt_us.load(Ordering::Relaxed) as f32 / 1000.0;
            let title = format!(
                "Remote Mac — {:.0} fps (показано {:.0}) — RTT {:.1} мс{}",
                (decoded - last_decoded) as f32 / secs,
                (shown - last_shown) as f32 / secs,
                rtt,
                if grabbed { " — ввод захвачен (Ctrl+Alt+G)" } else { "" }
            );
            let _ = canvas.window_mut().set_title(&title);
            last_decoded = decoded;
            last_shown = shown;
            last_stats = Instant::now();
        }
    }

    for code in held.drain() {
        tx.send(&Message::Key { code, down: false });
    }
    Ok(())
}

/// Отправляет последнее движение мыши, пересчитав координаты окна в долю видеокадра.
fn flush_move(
    tx: &mut Sender,
    pending: &mut Option<(i32, i32)>,
    canvas: &sdl2::render::Canvas<sdl2::video::Window>,
    vw: u32,
    vh: u32,
) {
    let Some((mx, my)) = pending.take() else {
        return;
    };
    if vw == 0 || vh == 0 {
        return;
    }
    let (ww, wh) = canvas.window().size();
    let (x, y, w, h) = video_rect(ww as f32, wh as f32, vw as f32, vh as f32);
    let nx = ((mx as f32 - x) / w).clamp(0.0, 1.0);
    let ny = ((my as f32 - y) / h).clamp(0.0, 1.0);
    tx.send(&Message::MouseMove { x: nx, y: ny });
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
