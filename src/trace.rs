//! W3C Trace Context propagation via MQTT v5 User Properties.
//!
//! Format: `00-{trace_id}-{span_id}-{flags}`
//! See: <https://www.w3.org/TR/trace-context/>

#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::fmt;

use crate::codec::properties::{Properties, PropertyId, PropertyValue};

const TRACEPARENT_KEY: &str = "traceparent";
const TRACESTATE_KEY: &str = "tracestate";
const VERSION: &str = "00";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub trace_flags: u8,
    pub tracestate: Option<String>,
}

impl TraceContext {
    /// Create a new root trace context from caller-provided random bytes.
    /// The caller is responsible for providing randomness (avoids pulling in a
    /// random number generator dependency).
    pub fn new_root(trace_id: [u8; 16], span_id: [u8; 8]) -> Self {
        Self {
            trace_id,
            span_id,
            trace_flags: 0x01, // sampled
            tracestate: None,
        }
    }

    /// Create a child span under this trace (new span_id, same trace_id).
    pub fn child(&self, span_id: [u8; 8]) -> Self {
        Self {
            trace_id: self.trace_id,
            span_id,
            trace_flags: self.trace_flags,
            tracestate: self.tracestate.clone(),
        }
    }

    /// Extract trace context from MQTT v5 User Properties on a received message.
    pub fn from_properties(props: &Properties) -> Option<Self> {
        let mut traceparent = None;
        let mut tracestate = None;

        for (key, value) in props.user_properties() {
            match key {
                TRACEPARENT_KEY => traceparent = Some(value),
                TRACESTATE_KEY => tracestate = Some(value),
                _ => {}
            }
        }

        let tp = traceparent?;
        Self::parse_traceparent(tp, tracestate)
    }

    /// Inject trace context into MQTT v5 User Properties for publishing.
    pub fn inject(&self, props: &mut Properties) {
        props.push(
            PropertyId::UserProperty,
            PropertyValue::StringPair(
                String::from(TRACEPARENT_KEY),
                self.traceparent_string(),
            ),
        );
        if let Some(ref state) = self.tracestate {
            props.push(
                PropertyId::UserProperty,
                PropertyValue::StringPair(
                    String::from(TRACESTATE_KEY),
                    state.clone(),
                ),
            );
        }
    }

    fn traceparent_string(&self) -> String {
        let mut s = String::with_capacity(55);
        s.push_str(VERSION);
        s.push('-');
        hex_encode(&self.trace_id, &mut s);
        s.push('-');
        hex_encode(&self.span_id, &mut s);
        s.push('-');
        hex_encode(&[self.trace_flags], &mut s);
        s
    }

    fn parse_traceparent(s: &str, tracestate: Option<&str>) -> Option<Self> {
        // Format: "00-{32 hex}-{16 hex}-{2 hex}"
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 4 || parts[0] != VERSION {
            return None;
        }

        let trace_id = hex_decode_fixed::<16>(parts[1])?;
        let span_id = hex_decode_fixed::<8>(parts[2])?;
        let flags_bytes = hex_decode_fixed::<1>(parts[3])?;

        Some(Self {
            trace_id,
            span_id,
            trace_flags: flags_bytes[0],
            tracestate: tracestate.map(String::from),
        })
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.traceparent_string())
    }
}

fn hex_encode(bytes: &[u8], out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
}

fn hex_decode_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut arr = [0u8; N];
    let bytes = s.as_bytes();
    for i in 0..N {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        arr[i] = (hi << 4) | lo;
    }
    Some(arr)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_round_trip() {
        let ctx = TraceContext::new_root(
            [0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb6, 0xa2, 0x7b,
             0xc4, 0xc9, 0x89, 0xd3, 0x5b, 0x8e, 0x7e, 0x00],
            [0xe4, 0x57, 0xb5, 0xa2, 0xe4, 0xd8, 0x6b, 0xd1],
        );
        let s = ctx.traceparent_string();
        assert_eq!(s, "00-4bf92f3577b6a27bc4c989d35b8e7e00-e457b5a2e4d86bd1-01");

        let parsed = TraceContext::parse_traceparent(&s, None).unwrap();
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
        assert_eq!(parsed.trace_flags, 0x01);
    }

    #[test]
    fn inject_extract_properties() {
        let ctx = TraceContext::new_root([0xAA; 16], [0xBB; 8]);
        let mut props = Properties::new();
        ctx.inject(&mut props);

        let extracted = TraceContext::from_properties(&props).unwrap();
        assert_eq!(extracted.trace_id, ctx.trace_id);
        assert_eq!(extracted.span_id, ctx.span_id);
    }

    #[test]
    fn child_preserves_trace_id() {
        let parent = TraceContext::new_root([0x11; 16], [0x22; 8]);
        let child = parent.child([0x33; 8]);
        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
    }

    #[test]
    fn invalid_traceparent() {
        assert!(TraceContext::parse_traceparent("garbage", None).is_none());
        assert!(TraceContext::parse_traceparent("00-short-bad-ff", None).is_none());
    }
}
