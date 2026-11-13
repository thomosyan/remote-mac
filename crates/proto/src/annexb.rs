//! Нарезка потока H.264 (Annex B) на access unit'ы (кадры).
//!
//! ffmpeg на хосте запускается с `h264_metadata=aud=insert`, поэтому каждый кадр
//! начинается с NAL-единицы типа 9 (access unit delimiter). Граница кадра — это
//! начало следующего AUD. Кадр отдаётся, когда появился AUD следующего,
//! то есть задержка этой нарезки — один кадр (около 16 мс при 60 fps).

pub struct AuSplitter {
    buf: Vec<u8>,
    /// true, если buf[0] — начало AUD (то есть начало кадра)
    head: bool,
}

impl Default for AuSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl AuSplitter {
    pub fn new() -> Self {
        AuSplitter {
            buf: Vec::with_capacity(512 * 1024),
            head: false,
        }
    }

    /// Добавляет данные из пайпа и возвращает все полностью принятые кадры.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        // Старт-код (3 байта) + байт типа NAL = 4 байта. Начало шаблона, у которого
        // не хватало хвоста в прошлый раз, лежит не раньше len-3.
        let from = self.buf.len().saturating_sub(3);
        self.buf.extend_from_slice(data);

        let b = &self.buf;
        let mut starts: Vec<usize> = Vec::new();
        if self.head {
            starts.push(0);
        }
        let mut i = from;
        while i + 3 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 && (b[i + 3] & 0x1f) == 9 {
                // 4-байтный старт-код 00 00 00 01: лишний ноль относим к этому кадру
                let s = if i > 0 && b[i - 1] == 0 { i - 1 } else { i };
                if s > 0 || !self.head {
                    starts.push(s);
                }
                i += 4;
            } else {
                i += 1;
            }
        }

        let mut out = Vec::new();
        for w in starts.windows(2) {
            out.push(self.buf[w[0]..w[1]].to_vec());
        }
        if let Some(&last) = starts.last() {
            self.buf.drain(..last);
            self.head = true;
        } else if !self.head && self.buf.len() > (1 << 20) {
            // AUD так и не встретился — это не тот поток, не копим бесконечно
            self.buf.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кадр: AUD + «SPS» + «слайс». Байты данных ненулевые, чтобы не имитировать старт-код.
    fn au(n: u8, four_byte: bool, size: usize) -> Vec<u8> {
        let mut v = Vec::new();
        if four_byte {
            v.push(0);
        }
        v.extend_from_slice(&[0, 0, 1, 0x09, 0xf0]); // AUD
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x11]); // SPS
        v.extend_from_slice(&[0, 0, 1, 0x65]); // IDR slice
        for k in 0..size {
            v.push(1 + ((k as u8).wrapping_add(n) % 200));
        }
        v
    }

    fn stream(aus: &[Vec<u8>]) -> Vec<u8> {
        aus.iter().flatten().copied().collect()
    }

    #[test]
    fn splits_with_any_chunking() {
        let aus: Vec<Vec<u8>> = (0..8)
            .map(|n| au(n, n % 2 == 0, 50 + n as usize * 300))
            .collect();
        let all = stream(&aus);

        for chunk in [1usize, 2, 3, 5, 7, 64, 1000, all.len()] {
            let mut sp = AuSplitter::new();
            let mut got: Vec<Vec<u8>> = Vec::new();
            for c in all.chunks(chunk) {
                got.extend(sp.push(c));
            }
            // последний кадр остаётся в буфере до следующего AUD
            assert_eq!(got.len(), aus.len() - 1, "chunk={chunk}");
            for (g, a) in got.iter().zip(aus.iter()) {
                assert_eq!(g, a, "chunk={chunk}");
            }
        }
    }

    #[test]
    fn last_frame_is_released_by_next_aud() {
        let a = au(1, true, 100);
        let b = au(2, true, 100);
        let mut sp = AuSplitter::new();
        assert!(sp.push(&a).is_empty());
        let got = sp.push(&b);
        assert_eq!(got, vec![a]);
    }

    #[test]
    fn garbage_before_first_aud_is_dropped() {
        let a = au(1, false, 40);
        let b = au(2, false, 40);
        let mut data = vec![7u8, 7, 7, 7, 7];
        data.extend_from_slice(&a);
        data.extend_from_slice(&b);
        let mut sp = AuSplitter::new();
        assert_eq!(sp.push(&data), vec![a]);
    }
}
