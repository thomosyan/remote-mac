//! Хост для macOS: захват экрана -> аппаратный H.264 (VideoToolbox) -> TCP,
//! и обратно: приём мыши и клавиатуры -> CGEvent.
//!
//! Захват и кодирование делает ffmpeg (avfoundation + h264_videotoolbox):
//! это тот же аппаратный энкодер Apple Silicon, который использует любое быстрое
//! решение. Rust отвечает за сеть, протокол, нарезку кадров и ввод.

mod input;

use proto::{read_message, tokens_equal, write_message, AuSplitter, Message, VERSION};
use std::io::{self, Read};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct Config {
    port: u16,
    token: String,
    screen: u32,
    width: u32,
    fps: u32,
    bitrate_mbit: u32,
}

const USAGE: &str = "\
remote-host — сервер удалённого управления (macOS)

Параметры:
  --port N        порт TCP (по умолчанию 7878)
  --token TEXT    токен доступа (по умолчанию случайный, печатается при запуске)
  --screen N      индекс экрана в avfoundation (по умолчанию 1);
                  список: ffmpeg -f avfoundation -list_devices true -i \"\"
  --width N       ширина потока в пикселях, высота по пропорциям (по умолчанию 1920)
  --fps N         кадров в секунду (по умолчанию 60)
  --bitrate N     битрейт, Мбит/с (по умолчанию 20)
";

fn value(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} требует значение"))
}

fn num<T: std::str::FromStr>(s: &str, name: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("{name}: неверное число «{s}»"))
}

fn random_token() -> String {
    let mut bytes = [0u8; 6];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !ok {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&t.to_le_bytes()[..6]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_args() -> Result<Config, String> {
    let mut port = 7878u16;
    let mut token: Option<String> = None;
    let mut screen = 1u32;
    let mut width = 1920u32;
    let mut fps = 60u32;
    let mut bitrate_mbit = 20u32;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => port = num(&value(&mut it, "--port")?, "--port")?,
            "--token" => token = Some(value(&mut it, "--token")?),
            "--screen" => screen = num(&value(&mut it, "--screen")?, "--screen")?,
            "--width" => width = num(&value(&mut it, "--width")?, "--width")?,
            "--fps" => fps = num(&value(&mut it, "--fps")?, "--fps")?,
            "--bitrate" => bitrate_mbit = num(&value(&mut it, "--bitrate")?, "--bitrate")?,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный параметр {other}\n\n{USAGE}")),
        }
    }
    if width < 320 || fps == 0 || bitrate_mbit == 0 {
        return Err("width/fps/bitrate заданы неверно".into());
    }
    Ok(Config {
        port,
        token: token.unwrap_or_else(random_token),
        screen,
        width,
        fps,
        bitrate_mbit,
    })
}

fn spawn_ffmpeg(cfg: &Config) -> io::Result<Child> {
    let fps = cfg.fps.to_string();
    let filter = format!(
        "scale={}:-2:flags=fast_bilinear,format=nv12",
        cfg.width & !1
    );
    let bitrate = format!("{}M", cfg.bitrate_mbit);
    let gop = (cfg.fps * 2).to_string();
    let input = format!("{}:none", cfg.screen);

    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "warning", "-nostdin"])
        // захват экрана вместе с курсором
        .args(["-f", "avfoundation", "-framerate", &fps])
        .args(["-capture_cursor", "1", "-i", &input])
        .args(["-vf", &filter])
        // постоянная частота кадров: на неподвижном экране кадры дублируются,
        // поэтому поток не «зависает» и нарезка по AUD не добавляет паузы
        .args(["-r", &fps, "-fps_mode", "cfr"])
        // аппаратный энкодер, режим минимальной задержки, без B-кадров
        .args(["-c:v", "h264_videotoolbox", "-realtime", "1", "-prio_speed", "1"])
        .args(["-profile:v", "high", "-bf", "0", "-g", &gop, "-b:v", &bitrate])
        // AUD в начале каждого кадра — по ним хост режет поток
        .args(["-bsf:v", "h264_metadata=aud=insert"])
        .args(["-flush_packets", "1", "-f", "h264", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

fn send_error(stream: &mut TcpStream, text: &str) {
    let _ = write_message(stream, &Message::Error(text.to_string()));
}

/// Рукопожатие: ждём Hello с верной версией и токеном.
fn handshake(stream: &mut TcpStream, cfg: &Config) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    match read_message(stream)? {
        Message::Hello { version, token } => {
            if version != VERSION {
                send_error(stream, "версии протокола не совпадают");
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad version"));
            }
            if !tokens_equal(&token, cfg.token.as_bytes()) {
                // небольшая пауза против перебора токена
                thread::sleep(Duration::from_secs(1));
                send_error(stream, "неверный токен");
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad token"));
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "первое сообщение должно быть Hello",
            ))
        }
    }
    write_message(stream, &Message::Welcome)?;
    stream.set_read_timeout(None)
}

/// Читает вывод ffmpeg, режет на кадры и шлёт клиенту. По завершении закрывает сокет,
/// чтобы цикл приёма ввода тоже остановился.
fn video_pump(mut out: impl Read, writer: Arc<Mutex<TcpStream>>, closing: Arc<AtomicBool>) {
    let mut splitter = AuSplitter::new();
    let mut buf = vec![0u8; 256 * 1024];
    let result = (|| -> io::Result<()> {
        loop {
            let n = out.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ffmpeg завершился (см. сообщения выше; проверьте разрешение на запись экрана)",
                ));
            }
            for au in splitter.push(&buf[..n]) {
                let mut w = writer.lock().unwrap();
                write_message(&mut *w, &Message::Video(au))?;
            }
        }
    })();
    // ошибка важна, только если сессия не закрывается штатно из-за отключения клиента
    if let Err(e) = result {
        if !closing.load(Ordering::SeqCst) {
            eprintln!("видео: {e}");
        }
    }
    let _ = writer.lock().unwrap().shutdown(Shutdown::Both);
}

fn handle_client(
    mut stream: TcpStream,
    cfg: &Config,
    injector: &mut input::Injector,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    handshake(&mut stream, cfg)?;
    eprintln!("клиент подключён, запускаю захват");

    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let mut ffmpeg = spawn_ffmpeg(cfg).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("не удалось запустить ffmpeg ({e}); установите: brew install ffmpeg"),
        )
    })?;
    let out = ffmpeg.stdout.take().expect("stdout piped");
    let closing = Arc::new(AtomicBool::new(false));
    let video = {
        let w = Arc::clone(&writer);
        let c = Arc::clone(&closing);
        thread::spawn(move || video_pump(out, w, c))
    };

    // приём ввода от клиента
    let result = loop {
        match read_message(&mut stream) {
            Ok(Message::Ping(v)) => {
                let mut w = writer.lock().unwrap();
                if let Err(e) = write_message(&mut *w, &Message::Pong(v)) {
                    break Err(e);
                }
            }
            Ok(msg) => injector.handle(&msg),
            Err(e) => break Err(e),
        }
    };

    closing.store(true, Ordering::SeqCst);
    injector.release_all();
    let _ = ffmpeg.kill();
    let _ = ffmpeg.wait();
    let _ = stream.shutdown(Shutdown::Both);
    let _ = video.join();
    result
}

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let mut injector = match input::Injector::new() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ввод недоступен: {e}");
            std::process::exit(1);
        }
    };

    let listener = match TcpListener::bind(("0.0.0.0", cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("не удалось занять порт {}: {e}", cfg.port);
            std::process::exit(1);
        }
    };

    eprintln!("слушаю порт {}", cfg.port);
    eprintln!("токен: {}", cfg.token);
    eprintln!(
        "поток: ширина {} px, {} fps, {} Мбит/с (трафик не шифруется — только для доверенной локальной сети)",
        cfg.width, cfg.fps, cfg.bitrate_mbit
    );

    // одно подключение за раз
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "?".into());
                eprintln!("подключение от {peer}");
                if let Err(e) = handle_client(stream, &cfg, &mut injector) {
                    eprintln!("сессия завершена: {e}");
                }
                eprintln!("жду следующее подключение");
            }
            Err(e) => eprintln!("ошибка accept: {e}"),
        }
    }
}
