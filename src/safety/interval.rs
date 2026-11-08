#[path = "interval_compact.rs"]
mod compact;
use compact::{Bits, Paths, Prefix};
use std::ops::Range;

#[derive(Default, Debug)]
pub struct Work {
    #[cfg(test)]
    pub preprocessing: usize,
    #[cfg(test)]
    pub authority_bytes: usize,
    #[cfg(test)]
    pub authority_scanned: usize,
    #[cfg(test)]
    pub parser_input_bytes: usize,
    #[cfg(test)]
    pub parser_calls: usize,
    #[cfg(test)]
    pub query_bytes: usize,
    #[cfg(test)]
    pub output_bytes: usize,
    #[cfg(test)]
    pub path_steps: usize,
}

pub struct Index<'a> {
    text: &'a str,
    next_query: Bits,
    next_fragment: Bits,
    next_amp: Bits,
    next_phone: Bits,
    non_control_end: Bits,
    event: Bits,
    path: Paths,
    query_encoded: Prefix,
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn escape(b: &[u8], i: usize) -> Option<u8> {
    (b.get(i) == Some(&b'%')).then_some(())?;
    Some(hex(*b.get(i + 1)?)? * 16 + hex(*b.get(i + 2)?)?)
}

fn phone_key(bytes: &[u8], mut i: usize, end: usize, _work: &mut Work) -> Option<usize> {
    let mut k = 0;
    while i < end && !matches!(bytes[i], b'=' | b'&' | b'#') {
        #[cfg(test)]
        {
            _work.query_bytes += 1;
        }
        if k == 5 {
            return None;
        }
        let (ch, step) = if i + 2 < end {
            escape(bytes, i).map_or((bytes[i], 1), |b| (b, 3))
        } else {
            (bytes[i], 1)
        };
        if ch != b"phone"[k] {
            return None;
        }
        k += 1;
        i += step;
    }
    if k != 5 {
        return None;
    }
    Some(if i < end && bytes[i] == b'=' {
        i + 1
    } else {
        i
    })
}

impl<'a> Index<'a> {
    pub fn new(text: &'a str, work: &mut Work) -> Self {
        let bytes = text.as_bytes();
        let n = bytes.len();
        let mut next_query = Bits::new(n);
        let mut next_fragment = Bits::new(n);
        let mut next_amp = Bits::new(n);
        let mut next_phone = Bits::new(n);
        let mut event = Bits::new(n);
        let mut non_control_end = Bits::new(n);
        let query_encoded = Prefix::new(bytes, false);
        for i in 0..n {
            if bytes[i] > 0x20 {
                non_control_end.set(i);
            }
            if bytes[i] == b'?' {
                next_query.set(i);
            }
            if bytes[i] == b'#' {
                next_fragment.set(i);
            }
            if bytes[i] == b'&' {
                next_amp.set(i);
                if phone_key(bytes, i + 1, n, work).is_some() {
                    next_phone.set(i);
                }
            }
            #[cfg(test)]
            {
                work.preprocessing += 1;
            }
        }
        let mut i = 0;
        while i < n {
            #[cfg(test)]
            {
                work.preprocessing += 1;
            }
            if let Some(ch) = escape(bytes, i) {
                if ch.is_ascii_digit() {
                    event.set(i);
                }
                i += 3;
            } else {
                if bytes[i].is_ascii_digit() {
                    event.set(i);
                }
                i += 1;
            }
        }
        for bits in [
            &mut next_query,
            &mut next_fragment,
            &mut next_amp,
            &mut next_phone,
            &mut non_control_end,
            &mut event,
        ] {
            bits.finish();
        }
        #[cfg(test)]
        let path = Paths::new(bytes, &mut work.path_steps);
        #[cfg(not(test))]
        let path = Paths::new(bytes, &mut 0);
        Self {
            text,
            next_query,
            next_fragment,
            next_amp,
            next_phone,
            non_control_end,
            event,
            path,
            query_encoded,
        }
    }

    pub fn query(&self, span: Range<usize>, work: &mut Work) -> Option<String> {
        self.query_with_limit(span, work, u32::MAX as u128)
    }

    pub fn query_with_limit(
        &self,
        span: Range<usize>,
        work: &mut Work,
        limit: u128,
    ) -> Option<String> {
        let b = self.text.as_bytes();
        let n = b.len();
        assert!(span.start <= span.end && span.end <= n);
        assert!(self.text.is_char_boundary(span.start) && self.text.is_char_boundary(span.end));
        assert!(self.text[span.start..].starts_with("whatsapp://"));
        let start = span.start + "whatsapp://".len();
        let end = self
            .non_control_end
            .previous(span.end)
            .map_or(0, |i| i + 1)
            .max(start);
        if end < start {
            return None;
        }
        let query = self.next_query.next(start);
        if query >= end || self.next_fragment.next(start) < query {
            return None;
        }
        let mut authority_end = start;
        while authority_end < end && !matches!(b[authority_end], b'/' | b'?' | b'#') {
            authority_end += 1;
            #[cfg(test)]
            {
                work.authority_bytes += 1;
            }
            #[cfg(test)]
            {
                work.authority_scanned += 1;
            }
        }
        let mut authority = String::from("whatsapp://");
        authority.push_str(&self.text[start..authority_end]);
        authority.push('/');
        #[cfg(test)]
        {
            work.authority_bytes += authority.len();
        }
        #[cfg(test)]
        {
            work.parser_input_bytes += authority.len();
        }
        #[cfg(test)]
        {
            work.parser_calls += 1;
        }
        let Ok(parsed) = url::Url::parse(&authority) else {
            return None;
        };
        if parsed.host_str() != Some("send") {
            return None;
        }
        let authority_len = parsed[..url::Position::BeforePath].len() as u128;
        let prefix = parsed
            .path()
            .strip_suffix('/')
            .expect("synthetic path ends with slash");
        let prefix_len = prefix.len() as u128;
        let path_len = if authority_end == query {
            prefix_len
        } else {
            let suffix = self
                .path
                .at(b, authority_end, &self.next_query, &self.next_fragment);
            let extra = if prefix_len == 0 || suffix.skip > 0 {
                0
            } else {
                prefix_len - u128::from(suffix.segments == 0)
            };
            suffix.path_len() + extra
        };
        if authority_len > limit || authority_len + path_len > limit {
            return None;
        }
        let query_end = end.min(self.next_fragment.next(query + 1));
        if query_end < end
            && authority_len + path_len + 1 + self.query_encoded.at(b, query_end)
                - self.query_encoded.at(b, query + 1)
                > limit
        {
            return None;
        }
        let first = query + 1;
        let value = match phone_key(b, first, query_end, work) {
            Some(v) => v,
            None => {
                let amp = self.next_phone.next(first);
                let p = if amp == n { n } else { amp + 1 };
                if p >= query_end {
                    return None;
                }
                phone_key(b, p, query_end, work)?
            }
        };
        let value_end = query_end.min(self.next_amp.next(value));
        let stable_end = value_end.saturating_sub(2).max(value);
        let mut digits = String::new();
        let mut at = value;
        while at < stable_end {
            at = self.event.next(at);
            if at >= stable_end {
                break;
            }
            #[cfg(test)]
            {
                work.query_bytes += 1;
            }
            digits.push(escape(b, at).unwrap_or(b[at]) as char);
            if digits.len() > 15 {
                return None;
            }
            at += 1;
        }
        for i in stable_end..value_end {
            #[cfg(test)]
            {
                work.query_bytes += 1;
            }
            let crossing = (i.saturating_sub(2)..=i)
                .find(|&p| p >= value && p + 2 >= value_end && escape(b, p).is_some());
            let digit = if crossing.is_some_and(|p| p < i) {
                b[i].is_ascii_digit().then_some(b[i])
            } else if crossing == Some(i) {
                None
            } else {
                self.event.contains(i).then(|| escape(b, i).unwrap_or(b[i]))
            };
            if let Some(ch) = digit {
                digits.push(ch as char);
            }
        }
        if !(7..=15).contains(&digits.len()) {
            return None;
        }
        #[cfg(test)]
        {
            work.output_bytes += digits.len();
        }
        Some(digits)
    }
}
