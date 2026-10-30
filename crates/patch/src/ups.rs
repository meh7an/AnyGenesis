//! UPS: the input and output sizes, then hunks of a skip and bytes XORed into the image up
//! to a zero byte, then the input, output and patch CRC-32s. Applied forward only.

use crate::{
    Format, PatchError, check_size,
    reader::{Footer, Reader},
};

pub(crate) const MAGIC: &[u8] = b"UPS1";

const FORMAT: Format = Format::Ups;

/// `image` with the UPS `patch` applied, after checking `image` is the patch's input.
pub(crate) fn apply(image: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    let (body, footer) = Footer::split(patch, FORMAT)?;
    let mut reader = Reader::new(&body[MAGIC.len()..], FORMAT);
    let input_size = reader.number()?;
    let output_size = check_size(reader.number()?)?;
    footer.check_source(FORMAT, image, input_size)?;
    // Input bytes past the input's end read as zero.
    let mut output: Vec<u8> = (0..output_size)
        .map(|offset| image.get(offset).copied().unwrap_or(0))
        .collect();
    let mut position = 0usize;
    while !reader.is_empty() {
        position = position
            .checked_add(reader.size()?)
            .ok_or(PatchError::Malformed {
                format: FORMAT,
                reason: "a hunk skips past any image",
            })?;
        // The zero that ends a hunk is itself an XOR by zero, so it moves the position too.
        loop {
            let xor = reader.byte()?;
            if xor != 0 {
                let byte = output.get_mut(position).ok_or(PatchError::Malformed {
                    format: FORMAT,
                    reason: "a hunk writes past the output's end",
                })?;
                *byte ^= xor;
            }
            position = position.saturating_add(1);
            if xor == 0 {
                break;
            }
        }
    }
    footer.check_target(FORMAT, &output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::encode;

    /// A UPS patch from `input` to `output`, one hunk per run of differing bytes.
    fn diff(input: &[u8], output: &[u8]) -> Vec<u8> {
        let mut body = MAGIC.to_vec();
        body.extend(encode::number(input.len() as u64));
        body.extend(encode::number(output.len() as u64));
        let xor: Vec<u8> = (0..output.len())
            .map(|offset| input.get(offset).copied().unwrap_or(0) ^ output[offset])
            .collect();
        let mut position = 0;
        while let Some(skip) = xor[position..].iter().position(|&byte| byte != 0) {
            body.extend(encode::number(skip as u64));
            position += skip;
            let run = xor[position..]
                .iter()
                .take_while(|&&byte| byte != 0)
                .count();
            body.extend(&xor[position..position + run]);
            body.push(0);
            position += run + 1;
            if position >= xor.len() {
                break;
            }
        }
        encode::finish(body, input, output)
    }

    #[test]
    fn test_apply_hunks_give_the_output() {
        let (input, output) = (b"the quick brown fox", b"the quack brewn fax");
        assert_eq!(apply(input, &diff(input, output)).expect("applies"), output);
    }

    #[test]
    fn test_apply_output_longer_or_shorter_than_input() {
        let (input, output) = (b"short".as_slice(), b"shorter, then longer".as_slice());
        assert_eq!(apply(input, &diff(input, output)).expect("grows"), output);
        assert_eq!(apply(output, &diff(output, input)).expect("shrinks"), input);
    }

    #[test]
    fn test_apply_hunk_ending_at_the_output_end() {
        let (input, output) = (b"abcd", b"abcz");
        assert_eq!(apply(input, &diff(input, output)).expect("applies"), output);
    }

    #[test]
    fn test_apply_wrong_or_patched_input_refused() {
        let (input, output) = (b"the quick brown fox", b"the quack brewn fax");
        let patch = diff(input, output);
        let error = apply(b"another image", &patch).expect_err("wrong input");
        assert!(matches!(error, PatchError::SourceCrc { .. }), "{error}");
        let error = apply(output, &patch).expect_err("already patched");
        assert!(
            matches!(error, PatchError::AlreadyPatched { .. }),
            "{error}"
        );
    }

    #[test]
    fn test_apply_hunk_past_the_output_refused() {
        let (input, output) = (b"abcd", b"abcd");
        let mut body = MAGIC.to_vec();
        body.extend(encode::number(4));
        body.extend(encode::number(4));
        body.extend(encode::number(4));
        body.extend([1, 0]);
        let error = apply(input, &encode::finish(body, input, output)).expect_err("past");
        assert!(matches!(error, PatchError::Malformed { .. }), "{error}");
    }
}
