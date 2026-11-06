//! Bounded parser for tmux control notifications. No client is started here.
//!
//! Output is sensitive terminal data. Callers must keep it in memory only and
//! discard their mirror on any decoding error, disconnect, or topology change.

const MAX_BUFFER: usize = 1024 * 1024;
const MAX_LINE: usize = 256 * 1024;
const MAX_EVENTS: usize = 4096;

/// Payloads may contain arbitrary bytes, including escape sequences and invalid UTF-8.
/// Intentionally does not implement Debug to avoid accidental terminal-data logging.
pub enum ControlEvent {
    Output { pane: u64, bytes: Vec<u8> },
    Paused { pane: u64 },
    Continued { pane: u64 },
    TopologyChanged,
    Closed,
}

#[derive(Default)]
pub struct ControlDecoder {
    pending: Vec<u8>,
    unusable: bool,
    response: Option<Vec<u8>>,
}

impl ControlDecoder {
    /// Accept arbitrary transport chunks. An error permanently poisons this
    /// decoder; callers must discard all prior output derived from this stream.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<ControlEvent>, &'static str> {
        if self.unusable {
            return Err("control stream closed or invalid");
        }
        if chunk.len() > MAX_BUFFER || self.pending.len().saturating_add(chunk.len()) > MAX_BUFFER {
            return self.fail("control buffer limit exceeded");
        }
        self.pending.extend_from_slice(chunk);
        let mut result = Vec::new();
        let mut consumed = 0;
        while let Some(relative) = self.pending[consumed..].iter().position(|b| *b == b'\n') {
            if relative > MAX_LINE || result.len() >= MAX_EVENTS {
                return self.fail("control frame limit exceeded");
            }
            let end = consumed + relative;
            let line = &self.pending[consumed..end];
            // tmux guarantees notifications do not occur inside command output.
            // Treat all response text as opaque, even if it resembles %output.
            if let Some(open) = &self.response {
                if let Some(close) = line
                    .strip_prefix(b"%end ")
                    .or_else(|| line.strip_prefix(b"%error "))
                {
                    if close != open.as_slice() {
                        return self.fail("mismatched control response delimiter");
                    }
                    self.response = None;
                }
                consumed = end + 1;
                continue;
            }
            if let Some(open) = line.strip_prefix(b"%begin ") {
                if !valid_response_id(open) {
                    return self.fail("invalid control response delimiter");
                }
                self.response = Some(open.to_vec());
                consumed = end + 1;
                continue;
            }
            if line.starts_with(b"%end ") || line.starts_with(b"%error ") || !line.starts_with(b"%")
            {
                return self.fail("unexpected control response line");
            }
            let event = match decode_line(line) {
                Ok(event) => event,
                Err(error) => return self.fail(error),
            };
            consumed = end + 1;
            if let Some(event) = event {
                let closed = matches!(event, ControlEvent::Closed);
                result.push(event);
                if closed {
                    self.unusable = true;
                    self.pending.clear();
                    return Ok(result);
                }
            }
        }
        if self.pending.len() - consumed > MAX_LINE {
            return self.fail("control frame limit exceeded");
        }
        self.pending.drain(..consumed);
        Ok(result)
    }

    /// A partial frame at EOF means the stream cannot be trusted.
    pub fn finish(&mut self) -> Result<(), &'static str> {
        if self.unusable || !self.pending.is_empty() || self.response.is_some() {
            return self.fail("control stream ended before a complete frame");
        }
        self.unusable = true;
        Ok(())
    }

    fn fail<T>(&mut self, error: &'static str) -> Result<T, &'static str> {
        self.pending.clear();
        self.unusable = true;
        Err(error)
    }
}

fn valid_response_id(raw: &[u8]) -> bool {
    let fields: Vec<_> = raw.split(|b| *b == b' ').collect();
    fields.len() == 3
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.iter().all(u8::is_ascii_digit))
}

fn pane_id(raw: &[u8]) -> Result<u64, &'static str> {
    let digits = raw.strip_prefix(b"%").ok_or("invalid pane id")?;
    if digits.is_empty() || digits.len() > 20 || !digits.iter().all(u8::is_ascii_digit) {
        return Err("invalid pane id");
    }
    std::str::from_utf8(digits)
        .map_err(|_| "invalid pane id")?
        .parse()
        .map_err(|_| "invalid pane id")
}

fn unescape(raw: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut result = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            let octal = raw.get(i + 1..i + 4).ok_or("truncated control escape")?;
            if !octal.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return Err("invalid control escape");
            }
            let value = ((octal[0] - b'0') as u16) * 64
                + ((octal[1] - b'0') as u16) * 8
                + (octal[2] - b'0') as u16;
            if value > u8::MAX as u16 {
                return Err("control escape out of range");
            }
            result.push(value as u8);
            i += 4;
        } else {
            result.push(raw[i]);
            i += 1;
        }
    }
    Ok(result)
}

fn decode_line(line: &[u8]) -> Result<Option<ControlEvent>, &'static str> {
    if let Some(rest) = line.strip_prefix(b"%output ") {
        let split = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or("invalid output frame")?;
        return Ok(Some(ControlEvent::Output {
            pane: pane_id(&rest[..split])?,
            bytes: unescape(&rest[split + 1..])?,
        }));
    }
    if let Some(rest) = line.strip_prefix(b"%extended-output ") {
        let split = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or("invalid extended output")?;
        let pane = pane_id(&rest[..split])?;
        let rest = &rest[split + 1..];
        let marker = rest
            .windows(3)
            .position(|x| x == b" : ")
            .ok_or("invalid extended output")?;
        let age = rest[..marker]
            .split(|b| *b == b' ')
            .next()
            .unwrap_or_default();
        if age.is_empty() || !age.iter().all(u8::is_ascii_digit) {
            return Err("invalid extended output age");
        }
        return Ok(Some(ControlEvent::Output {
            pane,
            bytes: unescape(&rest[marker + 3..])?,
        }));
    }
    if let Some(id) = line.strip_prefix(b"%pause ") {
        return Ok(Some(ControlEvent::Paused { pane: pane_id(id)? }));
    }
    if let Some(id) = line.strip_prefix(b"%continue ") {
        return Ok(Some(ControlEvent::Continued { pane: pane_id(id)? }));
    }
    if line == b"%exit" || line.starts_with(b"%exit ") {
        return Ok(Some(ControlEvent::Closed));
    }
    if [
        b"%session-changed ".as_slice(),
        b"%layout-change ",
        b"%window-close ",
        b"%unlinked-window-close ",
        b"%pane-mode-changed ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
    {
        return Ok(Some(ControlEvent::TopologyChanged));
    }
    // Never silently ignore a malformed data notification. Unknown control
    // notifications conservatively invalidate any mirror; command delimiters
    // and ordinary command-response lines carry no pane output.
    if line.starts_with(b"%output")
        || line.starts_with(b"%extended-output")
        || line.starts_with(b"%pause")
        || line.starts_with(b"%continue")
    {
        return Err("malformed control notification");
    }
    if line.starts_with(b"%") {
        return Ok(Some(ControlEvent::TopologyChanged));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_chunk_boundary_without_losing_terminal_bytes() {
        let wire = b"%begin 1 2 3\n%output %99 response text\n%end 1 2 3\n%output %12 A\\033[K\\011\\134\xc3\xb1\n%extended-output %12 5 future : B\\000\\012\n%pause %12\n%continue %12\n";
        for split in 0..=wire.len() {
            let mut decoder = ControlDecoder::default();
            let mut events = decoder.feed(&wire[..split]).unwrap();
            events.extend(decoder.feed(&wire[split..]).unwrap());
            assert_eq!(events.len(), 4);
            assert!(
                matches!(&events[0], ControlEvent::Output { pane: 12, bytes } if bytes == b"A\x1b[K\t\\\xc3\xb1")
            );
            assert!(
                matches!(&events[1], ControlEvent::Output { pane: 12, bytes } if bytes == b"B\0\n")
            );
            assert!(matches!(&events[2], ControlEvent::Paused { pane: 12 }));
            assert!(matches!(&events[3], ControlEvent::Continued { pane: 12 }));
            assert!(decoder.finish().is_ok());
            assert!(decoder.feed(b"%output %1 x\n").is_err());
        }
    }

    #[test]
    fn response_delimiters_are_matched_and_incomplete_responses_fail() {
        for wire in [
            b"%end 1 2 3\n".as_slice(),
            b"ordinary text\n",
            b"%begin invalid\n",
            b"%begin 1 2 3\n%end 1 2 4\n",
        ] {
            let mut decoder = ControlDecoder::default();
            assert!(decoder.feed(wire).is_err());
            assert!(decoder.feed(b"%output %1 x\n").is_err());
        }
        let mut decoder = ControlDecoder::default();
        assert!(
            decoder
                .feed(b"%begin 1 2 3\nresponse\n")
                .unwrap()
                .is_empty()
        );
        assert!(decoder.finish().is_err());
        let mut decoder = ControlDecoder::default();
        assert!(
            decoder
                .feed(b"%begin 1 2 3\nproblem\n%error 1 2 3\n")
                .unwrap()
                .is_empty()
        );
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn decodes_reference_captured_from_private_tmux() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/tmux-control-output.json"
        ))
        .unwrap();
        let decode_hex = |key: &str| -> Vec<u8> {
            let value = reference[key].as_str().unwrap().as_bytes();
            assert_eq!(value.len() % 2, 0);
            value
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        };
        let wire = decode_hex("wire_hex");
        let expected = decode_hex("payload_hex");
        for split in 0..=wire.len() {
            let mut decoder = ControlDecoder::default();
            let mut events = decoder.feed(&wire[..split]).unwrap();
            events.extend(decoder.feed(&wire[split..]).unwrap());
            assert_eq!(events.len(), 1);
            assert!(
                matches!(&events[0], ControlEvent::Output { pane: 0, bytes } if bytes == &expected)
            );
            assert!(decoder.finish().is_ok());
        }
    }

    #[cfg(feature = "ghostty")]
    #[test]
    fn captured_wire_renders_identically_to_original_terminal_bytes() {
        use crate::terminal_ghostty::GhosttyParser;

        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/tmux-control-output.json"
        ))
        .unwrap();
        let decode_hex = |key: &str| -> Vec<u8> {
            let value = reference[key].as_str().unwrap().as_bytes();
            assert_eq!(value.len() % 2, 0);
            value
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        };
        let wire = decode_hex("wire_hex");
        let payload = decode_hex("payload_hex");
        let mut direct = GhosttyParser::new(10, 40).unwrap();
        direct.process(&payload).unwrap();
        let expected = direct.snapshot(1, false, None).unwrap();
        for split in 0..=wire.len() {
            let mut decoder = ControlDecoder::default();
            let mut mirror = GhosttyParser::new(10, 40).unwrap();
            for chunk in [&wire[..split], &wire[split..]] {
                for event in decoder.feed(chunk).unwrap() {
                    match event {
                        ControlEvent::Output { pane: 0, bytes } => mirror.process(&bytes).unwrap(),
                        _ => panic!("unexpected synthetic control event"),
                    }
                }
            }
            decoder.finish().unwrap();
            let actual = mirror.snapshot(1, false, None).unwrap();
            assert_eq!(actual.cells, expected.cells, "split {split}");
            assert_eq!(actual.cursor, expected.cursor, "split {split}");
        }
    }

    #[test]
    fn rejects_malformed_escapes_ids_and_partial_eof_fail_closed() {
        for input in [
            b"%output %1 \\400\n".as_slice(),
            b"%output %1 \\09x\n",
            b"%output %-1 text\n",
            b"%output %18446744073709551616 x\n",
            b"%extended-output %1 nope : x\n",
            b"%output\n",
            b"%continue\n",
        ] {
            let mut decoder = ControlDecoder::default();
            assert!(decoder.feed(input).is_err());
            assert!(decoder.feed(b"%output %1 x\n").is_err());
        }
        let mut decoder = ControlDecoder::default();
        assert!(decoder.feed(b"%output %1 \\0").unwrap().is_empty());
        assert!(decoder.finish().is_err());
    }

    #[test]
    fn bounds_frames_and_signals_disconnect_and_topology_changes() {
        let mut decoder = ControlDecoder::default();
        assert!(decoder.feed(&vec![b'x'; MAX_LINE + 1]).is_err());
        let mut decoder = ControlDecoder::default();
        let events = decoder
            .feed(b"%session-changed $0 demo\n%layout-change @1 x\n%exit detached\n")
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], ControlEvent::TopologyChanged));
        assert!(matches!(events[1], ControlEvent::TopologyChanged));
        assert!(matches!(events[2], ControlEvent::Closed));
        let mut unknown = ControlDecoder::default();
        let events = unknown.feed(b"%future-data %1 something\n").unwrap();
        assert!(matches!(events[0], ControlEvent::TopologyChanged));
        assert!(decoder.feed(b"%output %1 x\n").is_err());
    }
}
