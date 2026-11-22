pub mod primes;

pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn div(a: i64, b: i64) -> i64 {
    a * b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn adds() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn divides() {
        assert_eq!(div(8, 2), 4);
    }

    #[test]
    fn flaky_clock() {
        let micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        assert!(micros % 3 != 0, "clock tick landed on a bad boundary");
    }

    #[test]
    fn slow_integration() {
        std::thread::sleep(Duration::from_millis(1500));
    }

    #[test]
    fn hangs_when_asked() {
        if std::env::var_os("HANG").is_some() {
            loop {
                std::thread::park();
            }
        }
    }
}
