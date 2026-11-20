//! Indexed fixture descriptors. The root authenticates the ordered list. A
//! lookup reads one bounded node at a time, including on the final short leaf.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PAGE_BYTES: usize = 16 * 1024;
const LEAF_ITEMS: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub count: usize,
    pub root: Option<String>,
}

fn is_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl List {
    pub fn valid(&self) -> bool {
        match &self.root {
            None => self.count == 0,
            Some(root) => self.count > 0 && is_digest(root),
        }
    }

    pub fn lookup(&self, index: usize) -> Result<Lookup, &'static str> {
        if !self.valid() || index >= self.count {
            return Err("fixture descriptor index");
        }
        Ok(Lookup {
            count: self.count,
            index,
            digest: self.root.clone().ok_or("fixture descriptor root")?,
        })
    }
}

pub struct Lookup {
    count: usize,
    index: usize,
    pub digest: String,
}

pub enum Read {
    Next(Lookup),
    Item(Value),
}

pub fn key(digest: &str) -> String {
    format!("metadata/{digest}.json")
}

impl Lookup {
    pub fn read(self, bytes: &[u8]) -> Result<Read, &'static str> {
        if bytes.len() > PAGE_BYTES || super::lane_logic::sha_hex(bytes) != self.digest {
            return Err("fixture descriptor page digest or size");
        }
        if self.count <= LEAF_ITEMS {
            let mut items: Vec<Value> =
                serde_json::from_slice(bytes).map_err(|_| "fixture descriptor leaf")?;
            if items.len() != self.count {
                return Err("fixture descriptor leaf length");
            }
            return Ok(Read::Item(items.swap_remove(self.index)));
        }
        let [left, right]: [String; 2] =
            serde_json::from_slice(bytes).map_err(|_| "fixture descriptor children")?;
        if !is_digest(&left) || !is_digest(&right) {
            return Err("fixture descriptor child digest");
        }
        let middle = self.count / 2;
        let (count, index, digest) = if self.index < middle {
            (middle, self.index, left)
        } else {
            (self.count - middle, self.index - middle, right)
        };
        Ok(Read::Next(Self {
            count,
            index,
            digest,
        }))
    }
}

#[cfg(test)]
pub fn write(items: &[Value], save: &mut impl FnMut(&str, &[u8])) -> List {
    fn subtree(items: &[Value], save: &mut impl FnMut(&str, &[u8])) -> String {
        let bytes = if items.len() <= LEAF_ITEMS {
            serde_json::to_vec(items).unwrap()
        } else {
            let (left, right) = items.split_at(items.len() / 2);
            serde_json::to_vec(&[subtree(left, save), subtree(right, save)]).unwrap()
        };
        assert!(
            bytes.len() <= PAGE_BYTES,
            "fixture descriptor page exceeds read bound"
        );
        let digest = super::lane_logic::sha_hex(&bytes);
        save(&key(&digest), &bytes);
        digest
    }
    List {
        count: items.len(),
        root: (!items.is_empty()).then(|| subtree(items, save)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn every_index_resolves_across_uneven_splits_and_binds_its_page() {
        for count in [0, 1, 32, 33, 65, 129, 5832] {
            let items: Vec<_> = (0..count).map(Value::from).collect();
            let mut pages = BTreeMap::new();
            let list = write(&items, &mut |key, body| {
                pages.insert(key.to_owned(), body.to_vec());
            });
            assert!(list.valid());
            assert!(list.lookup(count).is_err());
            for (index, expected) in items.iter().enumerate() {
                let mut cursor = list.lookup(index).unwrap();
                loop {
                    let body = &pages[&key(&cursor.digest)];
                    match cursor.read(body).unwrap() {
                        Read::Next(next) => cursor = next,
                        Read::Item(item) => {
                            assert_eq!(&item, expected);
                            break;
                        }
                    }
                }
            }
            if count > 0 {
                assert!(list.lookup(0).unwrap().read(b"[]").is_err());
            }
        }
    }
}
