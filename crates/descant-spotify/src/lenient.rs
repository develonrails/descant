//! Reading Spotify's JSON straight into the few fields Descant shows.
//!
//! Pathfinder's answers are large and their shapes drift, so they are not
//! read into a `serde_json::Value` tree first: each answer has its own small
//! structs naming only what is read, and everything else is skipped without
//! being built. Every field is read through [`Shape`], which takes a value of
//! the expected kind and reads past anything else as missing: a string where
//! a list was, or a null width, costs that one field, never the whole answer.

use std::marker::PhantomData;

use serde::de::{
    Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
    value::MapAccessDeserializer,
};

/// Something read from JSON that treats a value of the wrong kind as missing.
/// Each method takes one kind of JSON value; the defaults read past it.
pub(crate) trait Shape: Default + Sized {
    fn string(_: &str) -> Self {
        Self::default()
    }
    fn owned_string(s: String) -> Self {
        Self::string(&s)
    }
    /// A non-negative whole number.
    fn unsigned(_: u64) -> Self {
        Self::default()
    }
    /// A negative whole number.
    fn signed(_: i64) -> Self {
        Self::default()
    }
    fn boolean(_: bool) -> Self {
        Self::default()
    }
    fn object<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Self::default())
    }
    fn list<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Self::default())
    }
}

/// A string; anything else is none.
impl Shape for Option<String> {
    fn string(s: &str) -> Self {
        Some(s.to_owned())
    }
    fn owned_string(s: String) -> Self {
        Some(s)
    }
}

/// A non-negative whole number (as `Value::as_u64` reads it).
impl Shape for Option<u64> {
    fn unsigned(n: u64) -> Self {
        Some(n)
    }
}

/// A whole number that fits an `i64` (as `Value::as_i64` reads it).
impl Shape for Option<i64> {
    fn unsigned(n: u64) -> Self {
        i64::try_from(n).ok()
    }
    fn signed(n: i64) -> Self {
        Some(n)
    }
}

impl Shape for Option<bool> {
    fn boolean(b: bool) -> Self {
        Some(b)
    }
}

/// A list; an element of the wrong kind reads as its default.
impl<T: Shape> Shape for Vec<T> {
    fn list<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        let mut all = Vec::new();
        while let Some(Lenient(item)) = seq.next_element()? {
            all.push(item);
        }
        Ok(all)
    }
}

/// `T`, read leniently: what any JSON value deserializes to through [`Shape`].
pub(crate) struct Lenient<T>(pub T);

impl<'de, T: Shape> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ShapeVisitor(PhantomData)).map(Lenient)
    }
}

/// For `#[serde(deserialize_with = "…")]`.
pub(crate) fn lenient<'de, D: Deserializer<'de>, T: Shape>(d: D) -> Result<T, D::Error> {
    Lenient::deserialize(d).map(|Lenient(t)| t)
}

struct ShapeVisitor<T>(PhantomData<T>);

impl<'de, T: Shape> Visitor<'de> for ShapeVisitor<T> {
    type Value = T;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, b: bool) -> Result<T, E> {
        Ok(T::boolean(b))
    }
    fn visit_i64<E>(self, n: i64) -> Result<T, E> {
        Ok(match u64::try_from(n) {
            Ok(n) => T::unsigned(n),
            Err(_) => T::signed(n),
        })
    }
    fn visit_u64<E>(self, n: u64) -> Result<T, E> {
        Ok(T::unsigned(n))
    }
    fn visit_f64<E>(self, _: f64) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_str<E>(self, s: &str) -> Result<T, E> {
        Ok(T::string(s))
    }
    fn visit_string<E>(self, s: String) -> Result<T, E> {
        Ok(T::owned_string(s))
    }
    fn visit_unit<E>(self) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_none<E>(self) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<T, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
        T::object(map)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<T, A::Error> {
        T::list(seq)
    }
}

/// Reads a JSON object into a struct through its derived `Deserialize`.
pub(crate) fn object<'de, T, A>(map: A) -> Result<T, A::Error>
where
    T: Deserialize<'de>,
    A: MapAccess<'de>,
{
    T::deserialize(MapAccessDeserializer::new(map))
}

/// A struct read from a JSON object: camelCase keys, every field lenient and
/// missing ones default, other keys skipped.
macro_rules! shape {
    ($(#[$meta:meta])* $vis:vis struct $name:ident {
        $($(#[$fmeta:meta])* $field:ident: $ty:ty),* $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Default, serde::Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        $vis struct $name {
            $(
                $(#[$fmeta])*
                #[serde(deserialize_with = "crate::lenient::lenient")]
                pub(crate) $field: $ty,
            )*
        }

        impl $crate::lenient::Shape for $name {
            fn object<'de, A: serde::de::MapAccess<'de>>(map: A) -> Result<Self, A::Error> {
                $crate::lenient::object(map)
            }
        }
    };
}
pub(crate) use shape;

/// `{"items": [...]}`, as pathfinder wraps its lists.
#[derive(serde::Deserialize)]
#[serde(default, bound = "T: Shape")]
pub(crate) struct Items<T> {
    #[serde(deserialize_with = "lenient")]
    pub(crate) items: Vec<T>,
}

impl<T> Default for Items<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T: Shape> Shape for Items<T> {
    fn object<'de, A: MapAccess<'de>>(map: A) -> Result<Self, A::Error> {
        object(map)
    }
}

/// `{"data": ...}`, as pathfinder wraps most of what it names.
#[derive(Default, serde::Deserialize)]
#[serde(default, bound = "T: Shape")]
pub(crate) struct Data<T> {
    #[serde(deserialize_with = "lenient")]
    pub(crate) data: T,
}

impl<T: Shape> Shape for Data<T> {
    fn object<'de, A: MapAccess<'de>>(map: A) -> Result<Self, A::Error> {
        object(map)
    }
}

/// Reads JSON as `T` the way an answer is read: for tests.
#[cfg(test)]
pub(crate) fn read<T: Shape>(value: serde_json::Value) -> T {
    let bytes = serde_json::to_vec(&value).expect("JSON");
    serde_json::from_slice::<Lenient<T>>(&bytes)
        .expect("lenient reading takes any JSON")
        .0
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    shape! {
        struct Thing {
            name: Option<String>,
            width: Option<u64>,
            year: Option<i64>,
            playable: Option<bool>,
            tags: Vec<Option<String>>,
            inner: Items<Inner>,
            wrapped: Data<Inner>,
        }
    }

    shape! {
        struct Inner {
            uri: Option<String>,
        }
    }

    #[test]
    fn fields_of_the_right_kind_are_read() {
        let t: Thing = read(json!({
            "name": "Airbag", "width": 640, "year": 1997, "playable": false,
            "tags": ["a", "b"], "inner": {"items": [{"uri": "spotify:track:x"}]},
            "wrapped": {"data": {"uri": "spotify:album:y", "__typename": "Album"}},
            "sharingInfo": {"shareUrl": "https://open.spotify.com/…", "nested": [1, [2], {"x": null}]}
        }));
        assert_eq!(t.name.as_deref(), Some("Airbag"));
        assert_eq!(
            (t.width, t.year, t.playable),
            (Some(640), Some(1997), Some(false))
        );
        assert_eq!(t.tags, vec![Some("a".into()), Some("b".into())]);
        assert_eq!(t.inner.items[0].uri.as_deref(), Some("spotify:track:x"));
        assert_eq!(t.wrapped.data.uri.as_deref(), Some("spotify:album:y"));
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_missing_and_the_rest_still_reads() {
        let t: Thing = read(json!({
            "name": {"not": "a string"}, "width": "640", "year": -3, "playable": "yes",
            "tags": [1, "b", null, {"x": [1]}], "inner": {"items": "nope"},
            "wrapped": ["data"]
        }));
        assert_eq!(t.name, None);
        assert_eq!(t.width, None);
        assert_eq!(t.year, Some(-3), "a negative year is still a year");
        assert_eq!(t.playable, None);
        assert_eq!(t.tags, vec![None, Some("b".into()), None, None]);
        assert!(t.inner.items.is_empty());
        assert_eq!(t.wrapped.data.uri, None);
    }

    #[test]
    fn numbers_read_as_serde_json_values_read_them() {
        let t: Thing = read(json!({"width": -1, "year": u64::MAX}));
        assert_eq!(t.width, None, "no negative widths");
        assert_eq!(t.year, None, "too large for an i64");
        let t: Thing = read(json!({"width": 300.0, "year": 1997.5}));
        assert_eq!((t.width, t.year), (None, None), "fractions are not whole");
    }

    #[test]
    fn null_and_odd_roots_read_as_nothing() {
        for root in [json!(null), json!("x"), json!([1, 2]), json!(4)] {
            let t: Thing = read(root);
            assert_eq!(t.name, None);
            assert!(t.tags.is_empty());
        }
    }
}
