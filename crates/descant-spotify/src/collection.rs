//! Spotify's collection paging messages, encoded by hand.
//!
//! `collection2v2.proto` (Liked Songs, saved albums, followed artists) is not
//! among the protos librespot-protocol compiles, and these two messages are
//! small, so they are written and read field by field here:
//!
//! ```text
//! PageRequest   { 1 username, 2 set, 3 pagination_token: string; 4 limit: int32 }
//! PageResponse  { 1 items: repeated CollectionItem; 2 next_page_token, 3 sync_token: string }
//! CollectionItem{ 1 uri: string; 2 added_at: int32; 3 is_removed: bool; 4 context_uri: string }
//! ```

use protobuf::{CodedInputStream, CodedOutputStream, rt::WireType};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    pub username: String,
    pub set: String,
    pub pagination_token: String,
    pub limit: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionItem {
    pub uri: String,
    pub added_at: i32,
    pub is_removed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageResponse {
    pub items: Vec<CollectionItem>,
    pub next_page_token: String,
}

impl PageRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut out = CodedOutputStream::vec(&mut bytes);
            // proto3: default values are left out
            let strings = [
                (1, &self.username),
                (2, &self.set),
                (3, &self.pagination_token),
            ];
            for (field, value) in strings {
                if !value.is_empty() {
                    out.write_string(field, value).expect("writing to a Vec");
                }
            }
            if self.limit != 0 {
                out.write_int32(4, self.limit).expect("writing to a Vec");
            }
            out.flush().expect("writing to a Vec");
        }
        bytes
    }
}

/// A `WriteRequest` saving (`saved`) or removing one item from a set:
/// `{1 username, 2 set, 3 CollectionItem}`, where a saved item carries the
/// time it was added (seconds) and a removed one `is_removed`.
pub fn write_request(username: &str, set: &str, uri: &str, saved: bool, now_secs: i64) -> Vec<u8> {
    let mut item = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut item);
        out.write_string(1, uri).expect("writing to a Vec");
        if saved {
            out.write_int64(2, now_secs).expect("writing to a Vec");
        } else {
            out.write_bool(3, true).expect("writing to a Vec");
        }
        out.flush().expect("writing to a Vec");
    }
    let mut bytes = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut bytes);
        out.write_string(1, username).expect("writing to a Vec");
        out.write_string(2, set).expect("writing to a Vec");
        out.write_bytes(3, &item).expect("writing to a Vec");
        out.flush().expect("writing to a Vec");
    }
    bytes
}

impl PageResponse {
    pub fn decode(bytes: &[u8]) -> protobuf::Result<PageResponse> {
        let mut input = CodedInputStream::from_bytes(bytes);
        let mut page = PageResponse::default();
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            match (tag >> 3, tag & 7) {
                (1, 2) => {
                    let length = input.read_raw_varint64()?;
                    let outer = input.push_limit(length)?;
                    page.items.push(read_item(&mut input)?);
                    input.pop_limit(outer);
                }
                (2, 2) => page.next_page_token = input.read_string()?,
                (_, wire) => skip(&mut input, wire)?,
            }
        }
        Ok(page)
    }
}

fn read_item(input: &mut CodedInputStream) -> protobuf::Result<CollectionItem> {
    let mut item = CollectionItem::default();
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match (tag >> 3, tag & 7) {
            (1, 2) => item.uri = input.read_string()?,
            (2, 0) => item.added_at = input.read_int32()?,
            (3, 0) => item.is_removed = input.read_bool()?,
            (_, wire) => skip(input, wire)?,
        }
    }
    Ok(item)
}

fn skip(input: &mut CodedInputStream, wire: u32) -> protobuf::Result<()> {
    let wire = WireType::new(wire).ok_or(protobuf::Error::from(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "unknown wire type",
    )))?;
    input.skip_field(wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_bytes(uri: &str, added_at: i32, removed: bool, context: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut out = CodedOutputStream::vec(&mut bytes);
        out.write_string(1, uri).unwrap();
        out.write_int32(2, added_at).unwrap();
        out.write_bool(3, removed).unwrap();
        out.write_string(4, context).unwrap();
        out.flush().unwrap();
        drop(out);
        bytes
    }

    #[test]
    fn a_page_response_reads_back() {
        let mut bytes = Vec::new();
        {
            let mut out = CodedOutputStream::vec(&mut bytes);
            out.write_bytes(
                1,
                &item_bytes("spotify:track:a", 1_700_000_000, false, "ctx"),
            )
            .unwrap();
            out.write_bytes(1, &item_bytes("spotify:album:b", 1_600_000_000, true, ""))
                .unwrap();
            out.write_string(2, "next").unwrap();
            out.write_string(3, "sync-token-ignored").unwrap();
            out.flush().unwrap();
        }
        let page = PageResponse::decode(&bytes).unwrap();
        assert_eq!(
            page,
            PageResponse {
                items: vec![
                    CollectionItem {
                        uri: "spotify:track:a".into(),
                        added_at: 1_700_000_000,
                        is_removed: false
                    },
                    CollectionItem {
                        uri: "spotify:album:b".into(),
                        added_at: 1_600_000_000,
                        is_removed: true
                    },
                ],
                next_page_token: "next".into(),
            }
        );
    }

    #[test]
    fn a_page_request_writes_what_is_set() {
        let request = PageRequest {
            username: "u".into(),
            set: "collection".into(),
            pagination_token: String::new(),
            limit: 200,
        };
        let bytes = request.encode();
        // username, set, limit; the empty token is left out
        let mut input = CodedInputStream::from_bytes(&bytes);
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(1 << 3 | 2));
        assert_eq!(input.read_string().unwrap(), "u");
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(2 << 3 | 2));
        assert_eq!(input.read_string().unwrap(), "collection");
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(4 << 3));
        assert_eq!(input.read_int32().unwrap(), 200);
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), None);
    }

    #[test]
    fn rubbish_is_an_error_not_a_panic() {
        assert!(PageResponse::decode(&[0x0a, 0xff, 0xff]).is_err());
    }

    #[test]
    fn a_write_request_saves_with_a_time_and_removes_with_a_flag() {
        let read_item = |bytes: &[u8]| {
            let mut input = CodedInputStream::from_bytes(bytes);
            assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(1 << 3 | 2));
            assert_eq!(input.read_string().unwrap(), "me");
            assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(2 << 3 | 2));
            assert_eq!(input.read_string().unwrap(), "collection");
            assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(3 << 3 | 2));
            let length = input.read_raw_varint64().unwrap();
            let outer = input.push_limit(length).unwrap();
            let item = read_item(&mut input).unwrap();
            input.pop_limit(outer);
            item
        };
        let saved = read_item(&write_request(
            "me",
            "collection",
            "spotify:track:a",
            true,
            1_700_000_000,
        ));
        assert_eq!(
            saved,
            CollectionItem {
                uri: "spotify:track:a".into(),
                added_at: 1_700_000_000,
                is_removed: false
            }
        );
        let removed = read_item(&write_request(
            "me",
            "collection",
            "spotify:track:a",
            false,
            0,
        ));
        assert!(removed.is_removed);
    }
}
