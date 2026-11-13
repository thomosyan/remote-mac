//! Физические клавиши SDL (scancode) -> виртуальные коды macOS (kVK_*).
//! Передаётся именно положение клавиши, а не символ: раскладку выбирает сам Mac.

use sdl2::keyboard::Scancode;

/// `ctrl_as_cmd`: Ctrl на Windows работает как Cmd на Mac (Ctrl+C -> Cmd+C),
/// а клавиша Win работает как Control.
pub fn to_mac(sc: Scancode, ctrl_as_cmd: bool) -> Option<u16> {
    use Scancode::*;
    let code = match sc {
        A => 0,
        S => 1,
        D => 2,
        F => 3,
        H => 4,
        G => 5,
        Z => 6,
        X => 7,
        C => 8,
        V => 9,
        B => 11,
        Q => 12,
        W => 13,
        E => 14,
        R => 15,
        Y => 16,
        T => 17,
        Num1 => 18,
        Num2 => 19,
        Num3 => 20,
        Num4 => 21,
        Num6 => 22,
        Num5 => 23,
        Equals => 24,
        Num9 => 25,
        Num7 => 26,
        Minus => 27,
        Num8 => 28,
        Num0 => 29,
        RightBracket => 30,
        O => 31,
        U => 32,
        LeftBracket => 33,
        I => 34,
        P => 35,
        Return => 36,
        L => 37,
        J => 38,
        Apostrophe => 39,
        K => 40,
        Semicolon => 41,
        Backslash => 42,
        Comma => 43,
        Slash => 44,
        N => 45,
        M => 46,
        Period => 47,
        Tab => 48,
        Space => 49,
        Grave => 50,
        Backspace => 51,
        Escape => 53,
        CapsLock => 57,
        NonUsBackslash => 10,

        // модификаторы: 55/54 Cmd, 56/60 Shift, 58/61 Option, 59/62 Control
        LShift => 56,
        RShift => 60,
        LAlt => 58,
        RAlt => 61,
        LCtrl => {
            if ctrl_as_cmd {
                55
            } else {
                59
            }
        }
        RCtrl => {
            if ctrl_as_cmd {
                54
            } else {
                62
            }
        }
        LGui => {
            if ctrl_as_cmd {
                59
            } else {
                55
            }
        }
        RGui => {
            if ctrl_as_cmd {
                62
            } else {
                54
            }
        }

        F1 => 122,
        F2 => 120,
        F3 => 99,
        F4 => 118,
        F5 => 96,
        F6 => 97,
        F7 => 98,
        F8 => 100,
        F9 => 101,
        F10 => 109,
        F11 => 103,
        F12 => 111,

        Home => 115,
        PageUp => 116,
        Delete => 117,
        End => 119,
        PageDown => 121,
        Insert => 114,
        Left => 123,
        Right => 124,
        Down => 125,
        Up => 126,

        KpPeriod => 65,
        KpMultiply => 67,
        KpPlus => 69,
        KpDivide => 75,
        KpEnter => 76,
        KpMinus => 78,
        Kp0 => 82,
        Kp1 => 83,
        Kp2 => 84,
        Kp3 => 85,
        Kp4 => 86,
        Kp5 => 87,
        Kp6 => 88,
        Kp7 => 89,
        Kp8 => 91,
        Kp9 => 92,
        _ => return None,
    };
    Some(code)
}
