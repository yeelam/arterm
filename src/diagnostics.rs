use std::{
    fmt,
    io::{self, Write},
};

pub fn line(args: fmt::Arguments<'_>) {
    let output = normalize(&args.to_string());
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(&output);
    let _ = stderr.flush();
}

fn normalize(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() + 2);
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                output.extend_from_slice(b"\r\n");
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            b'\n' => {
                output.extend_from_slice(b"\r\n");
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    if !output.ends_with(b"\r\n") {
        output.extend_from_slice(b"\r\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_use_crlf_without_a_leading_carriage_return() {
        assert_eq!(normalize("status"), b"status\r\n");
        assert_eq!(normalize("one\ntwo"), b"one\r\ntwo\r\n");
        assert_eq!(normalize("one\r\ntwo\rthree"), b"one\r\ntwo\r\nthree\r\n");
        assert!(!normalize("status").starts_with(b"\r"));
    }
}
