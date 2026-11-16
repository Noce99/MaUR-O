//! The history of a MOT map: who changed what, and the map as it was.
//!
//! See `mds/MOT-Format.md`. Every object has a stable id (the same in every tile
//! it is cut into) and goes through *versions*, each valid from one changeset to
//! another, the current one to none. Changing an object closes its version and
//! opens a new one; nothing is deleted, and undoing a change is another change.
//! This is the model of OpenStreetMap's full-history data.
//!
//! A tile's versions are kept as two byte strings:
//!
//! * the **head**: the current objects, as the plain tile payload of
//!   [`crate::mot`], so a viewer which knows nothing of history reads it as it
//!   always did and pays nothing for there being any;
//! * the **history** sidecar: the versions that have been closed, and when each
//!   current object was born. Read only to go back in time.
//!
//! The changesets themselves (who, when, why) are in a [`ChangeLog`], one for the
//! whole map; a version holds only the id of its changeset, one number.
//!
//! This module is the pure part: the versions of one tile and what a change does
//! to them. Where the head and the sidecar are stored (the sidecar as an archive
//! of its own, say), who may change what, and conflicting changes are the
//! application's.

use std::collections::BTreeMap;

use crate::mot::{Tile, TileObject};

/// The id of a changeset. Changesets are numbered in the order they were made,
/// from 1; 0 stands for the map as it was first stored, before any.
pub type ChangesetId = u32;

/// One change to the map: who made it, when, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changeset {
    /// Its number.
    pub id: ChangesetId,
    /// When it was made, in seconds since the Unix epoch.
    pub time: i64,
    /// Who made it: an id the application knows, opaque here.
    pub author: u32,
    /// The author's account of it.
    pub comment: String,
    /// What was used to make it ("Mapper", "omap_to_tiles").
    pub tool: String,
}

/// The changesets of a map, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeLog {
    /// The changesets, in the order they were made.
    pub changesets: Vec<Changeset>,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// A cursor over bytes which says what ran out and never panics.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let b = *self.bytes.get(self.at).ok_or("history ends too soon")?;
            self.at += 1;
            if shift == 63 && b > 1 {
                return Err("varint too long".into());
            }
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint too long".into())
    }

    fn signed(&mut self) -> Result<i64, String> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("length overflows")?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or("history ends too soon")?;
        self.at = end;
        Ok(slice)
    }

    fn string(&mut self) -> Result<String, String> {
        let n = usize::try_from(self.varint()?).map_err(|_| "string too long")?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "text is not UTF-8".into())
    }

    fn id(&mut self) -> Result<ChangesetId, String> {
        ChangesetId::try_from(self.varint()?).map_err(|_| "changeset id out of range".into())
    }
}

impl ChangeLog {
    /// The changeset with this id.
    pub fn get(&self, id: ChangesetId) -> Option<&Changeset> {
        self.changesets.iter().find(|c| c.id == id)
    }

    /// The last changeset made at or before `time`: the one which says what the
    /// map was at that moment. `None` before the first.
    pub fn at_time(&self, time: i64) -> Option<ChangesetId> {
        self.changesets
            .iter()
            .filter(|c| c.time <= time)
            .max_by_key(|c| c.id)
            .map(|c| c.id)
    }

    /// The log as bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, self.changesets.len() as u64);
        let (mut previous_id, mut previous_time) = (0i64, 0i64);
        for c in &self.changesets {
            put_varint(&mut out, zigzag(i64::from(c.id) - previous_id));
            put_varint(&mut out, zigzag(c.time - previous_time));
            put_varint(&mut out, u64::from(c.author));
            for text in [&c.comment, &c.tool] {
                put_varint(&mut out, text.len() as u64);
                out.extend_from_slice(text.as_bytes());
            }
            (previous_id, previous_time) = (i64::from(c.id), c.time);
        }
        out
    }

    /// A log written by [`ChangeLog::encode`]; an error, never a panic, for
    /// bytes that are not one.
    pub fn decode(bytes: &[u8]) -> Result<ChangeLog, String> {
        let mut r = Reader { bytes, at: 0 };
        let count = usize::try_from(r.varint()?).map_err(|_| "too many changesets")?;
        // A changeset takes at least five bytes.
        if count > bytes.len() {
            return Err("changeset count exceeds the size".into());
        }
        let mut changesets = Vec::with_capacity(count);
        let (mut id, mut time) = (0i64, 0i64);
        for _ in 0..count {
            id = id
                .checked_add(r.signed()?)
                .ok_or("changeset id out of range")?;
            time = time.checked_add(r.signed()?).ok_or("time out of range")?;
            changesets.push(Changeset {
                id: ChangesetId::try_from(id).map_err(|_| "changeset id out of range")?,
                time,
                author: u32::try_from(r.varint()?).map_err(|_| "author out of range")?,
                comment: r.string()?,
                tool: r.string()?,
            });
        }
        Ok(ChangeLog { changesets })
    }
}

/// One version of an object in a tile: valid from one changeset up to, but not
/// including, another.
#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    /// The object as it is in this version.
    pub object: TileObject,
    /// The changeset which made this version.
    pub from: ChangesetId,
    /// The changeset which replaced or removed it, `None` for the current one.
    pub to: Option<ChangesetId>,
}

impl Version {
    fn valid_at(&self, changeset: ChangesetId) -> bool {
        self.from <= changeset && self.to.is_none_or(|to| changeset < to)
    }
}

/// What a changeset does to one object of a tile.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A new object (it has an id of its own). One object can be in a tile in
    /// several pieces, all with its id: add them all.
    Add(Vec<TileObject>),
    /// The object with this id is gone.
    Remove(u64),
    /// The object with this id becomes these pieces.
    Replace(u64, Vec<TileObject>),
}

/// The versions of the objects of one tile.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionedTile {
    /// The resolution of the nodes, as in [`Tile::grid_cm`].
    pub grid_cm: u32,
    /// Every version: the current ones and the closed.
    pub versions: Vec<Version>,
}

impl VersionedTile {
    /// A tile with no history: its objects as they were first stored, which is
    /// as of changeset `born` (0 for the map as it came).
    pub fn new(tile: Tile, born: ChangesetId) -> VersionedTile {
        VersionedTile {
            grid_cm: tile.grid_cm,
            versions: tile
                .objects
                .into_iter()
                .map(|object| Version {
                    object,
                    from: born,
                    to: None,
                })
                .collect(),
        }
    }

    /// The tile as it is now.
    pub fn head(&self) -> Tile {
        self.at(ChangesetId::MAX)
    }

    /// The tile as it was once changeset `changeset` had been made.
    pub fn at(&self, changeset: ChangesetId) -> Tile {
        Tile {
            grid_cm: self.grid_cm,
            objects: self
                .versions
                .iter()
                .filter(|v| v.valid_at(changeset))
                .map(|v| v.object.clone())
                .collect(),
        }
    }

    /// The newest changeset any version mentions: a change has to be later.
    fn latest(&self) -> ChangesetId {
        self.versions
            .iter()
            .flat_map(|v| [v.from, v.to.unwrap_or(0)])
            .max()
            .unwrap_or(0)
    }

    /// Makes changeset `changeset`: its `changes` close the versions they
    /// replace or remove and open the new ones. An error, and nothing done,
    /// where the changeset is not later than the last, a change names an object
    /// that is not there, or an object added already is.
    pub fn apply(&mut self, changeset: ChangesetId, changes: Vec<Change>) -> Result<(), String> {
        if changeset <= self.latest() {
            return Err(format!(
                "changeset {changeset} is not later than {}",
                self.latest()
            ));
        }
        let mut next = self.versions.clone();
        let alive = |versions: &[Version], id: u64| {
            versions.iter().any(|v| v.to.is_none() && v.object.id == id)
        };
        for change in changes {
            let (closing, adding) = match change {
                Change::Add(objects) => (None, objects),
                Change::Remove(id) => (Some(id), Vec::new()),
                Change::Replace(id, objects) => (Some(id), objects),
            };
            if let Some(id) = closing {
                if !alive(&next, id) {
                    return Err(format!("object {id} is not in the tile"));
                }
                for v in next
                    .iter_mut()
                    .filter(|v| v.to.is_none() && v.object.id == id)
                {
                    v.to = Some(changeset);
                }
            }
            for object in adding {
                // An add of an id that is there is a mistake; a replace has
                // just closed its own.
                if closing.is_none() && alive(&next, object.id) {
                    return Err(format!("object {} is already in the tile", object.id));
                }
                next.push(Version {
                    object,
                    from: changeset,
                    to: None,
                });
            }
        }
        self.versions = next;
        Ok(())
    }

    /// The changes which put the tile back as it was before changeset `target`
    /// was made, to be made as a changeset of their own: undoing is a new change
    /// and the history keeps both. The objects it brought are removed and the
    /// ones it removed or replaced come back as they were.
    pub fn revert(&self, target: ChangesetId) -> Vec<Change> {
        let mut changes = Vec::new();
        let mut by_id: BTreeMap<u64, (Vec<TileObject>, Vec<TileObject>)> = BTreeMap::new();
        for v in &self.versions {
            if v.from == target && v.to.is_none() {
                by_id
                    .entry(v.object.id)
                    .or_default()
                    .0
                    .push(v.object.clone());
            }
            if v.to == Some(target) {
                by_id
                    .entry(v.object.id)
                    .or_default()
                    .1
                    .push(v.object.clone());
            }
        }
        for (id, (brought, taken)) in by_id {
            match (brought.is_empty(), taken.is_empty()) {
                (false, true) => changes.push(Change::Remove(id)),
                (false, false) => changes.push(Change::Replace(id, taken)),
                (true, false) => changes.push(Change::Add(taken)),
                (true, true) => {}
            }
        }
        changes
    }

    /// The head: the current objects, as a plain tile payload.
    pub fn encode_head(&self) -> Vec<u8> {
        self.head().encode()
    }

    /// The history sidecar: when each current object was born (unless it is as
    /// it was first stored) and every closed version.
    pub fn encode_history(&self) -> Vec<u8> {
        let mut births: BTreeMap<u64, ChangesetId> = BTreeMap::new();
        for v in self
            .versions
            .iter()
            .filter(|v| v.to.is_none() && v.from != 0)
        {
            births.insert(v.object.id, v.from);
        }
        let mut closed: Vec<&Version> = self.versions.iter().filter(|v| v.to.is_some()).collect();
        closed.sort_by_key(|v| (v.object.symbol, v.object.id, v.from));

        let mut out = Vec::new();
        put_varint(&mut out, births.len() as u64);
        let mut previous = 0u64;
        for (&id, &from) in &births {
            put_varint(&mut out, id - previous);
            put_varint(&mut out, u64::from(from));
            previous = id;
        }
        let tile = Tile {
            grid_cm: self.grid_cm,
            objects: closed.iter().map(|v| v.object.clone()).collect(),
        };
        let payload = tile.encode();
        put_varint(&mut out, payload.len() as u64);
        out.extend_from_slice(&payload);
        put_varint(&mut out, closed.len() as u64);
        for v in &closed {
            put_varint(&mut out, u64::from(v.from));
        }
        for v in &closed {
            put_varint(&mut out, u64::from(v.to.unwrap_or(0)));
        }
        out
    }

    /// The versions from a head and, where there is one, its history sidecar.
    /// An error, never a panic, for bytes that are not ones.
    pub fn decode(head: &[u8], history: Option<&[u8]>) -> Result<VersionedTile, String> {
        let head = Tile::decode(head)?;
        let mut versions: Vec<Version> = head
            .objects
            .into_iter()
            .map(|object| Version {
                object,
                from: 0,
                to: None,
            })
            .collect();
        let grid_cm = head.grid_cm;
        let Some(bytes) = history else {
            return Ok(VersionedTile { grid_cm, versions });
        };
        let mut r = Reader { bytes, at: 0 };
        let births = usize::try_from(r.varint()?).map_err(|_| "too many births")?;
        if births > bytes.len() {
            return Err("birth count exceeds the size".into());
        }
        let mut born: BTreeMap<u64, ChangesetId> = BTreeMap::new();
        let mut id = 0u64;
        for _ in 0..births {
            id = id
                .checked_add(r.varint()?)
                .ok_or("object id out of range")?;
            born.insert(id, r.id()?);
        }
        for v in &mut versions {
            v.from = born.get(&v.object.id).copied().unwrap_or(0);
        }
        let length = usize::try_from(r.varint()?).map_err(|_| "payload too long")?;
        let closed = Tile::decode(r.take(length)?)?;
        if closed.grid_cm != grid_cm && !closed.objects.is_empty() {
            return Err("the history is on another grid than the head".into());
        }
        let count = usize::try_from(r.varint()?).map_err(|_| "too many versions")?;
        if count != closed.objects.len() {
            return Err("the history's versions do not match its objects".into());
        }
        let froms = (0..count).map(|_| r.id()).collect::<Result<Vec<_>, _>>()?;
        let tos = (0..count).map(|_| r.id()).collect::<Result<Vec<_>, _>>()?;
        for ((object, from), to) in closed.objects.into_iter().zip(froms).zip(tos) {
            versions.push(Version {
                object,
                from,
                to: Some(to),
            });
        }
        Ok(VersionedTile { grid_cm, versions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mot::{Node, Shape};

    fn object(id: u64, symbol: u16, x: i32) -> TileObject {
        TileObject {
            id,
            symbol,
            rotation: 0.0,
            shape: Shape::Path(None),
            nodes: vec![
                Node { x, y: 0, flags: 0 },
                Node {
                    x: x + 10,
                    y: 5,
                    flags: 0,
                },
            ],
        }
    }

    fn ids(tile: &Tile) -> Vec<(u64, i32)> {
        let mut v: Vec<(u64, i32)> = tile.objects.iter().map(|o| (o.id, o.nodes[0].x)).collect();
        v.sort_unstable();
        v
    }

    fn start() -> VersionedTile {
        let mut tile = Tile::new(5);
        tile.objects.push(object(1, 3, 100));
        tile.objects.push(object(2, 3, 200));
        tile.objects.push(object(3, 7, 300));
        VersionedTile::new(tile, 0)
    }

    #[test]
    fn a_change_closes_a_version_and_opens_another_and_the_past_stays() {
        let mut t = start();
        // 1: object 2 moves, object 3 goes, object 4 is new.
        t.apply(
            1,
            vec![
                Change::Replace(2, vec![object(2, 3, 250)]),
                Change::Remove(3),
                Change::Add(vec![object(4, 7, 400)]),
            ],
        )
        .unwrap();
        assert_eq!(ids(&t.head()), [(1, 100), (2, 250), (4, 400)]);
        assert_eq!(
            ids(&t.at(0)),
            [(1, 100), (2, 200), (3, 300)],
            "as first stored"
        );
        // 2: the new object is moved again.
        t.apply(2, vec![Change::Replace(4, vec![object(4, 7, 450)])])
            .unwrap();
        assert_eq!(ids(&t.at(1)), [(1, 100), (2, 250), (4, 400)]);
        assert_eq!(ids(&t.head()), [(1, 100), (2, 250), (4, 450)]);
        assert_eq!(t.versions.len(), 6);
    }

    #[test]
    fn a_change_that_cannot_be_made_changes_nothing() {
        let mut t = start();
        let before = t.clone();
        assert!(t.apply(1, vec![Change::Remove(99)]).is_err());
        assert!(t
            .apply(1, vec![Change::Add(vec![object(1, 3, 5)])])
            .is_err());
        // The second change is wrong, so the first is not made either.
        assert!(t
            .apply(1, vec![Change::Remove(1), Change::Remove(99)])
            .is_err());
        assert_eq!(t, before);
        t.apply(5, vec![Change::Remove(1)]).unwrap();
        assert!(t.apply(5, vec![Change::Remove(2)]).is_err(), "not later");
        assert!(t.apply(3, vec![Change::Remove(2)]).is_err(), "not later");
    }

    #[test]
    fn undoing_a_change_is_another_change_and_both_are_kept() {
        let mut t = start();
        t.apply(
            1,
            vec![
                Change::Replace(2, vec![object(2, 3, 250)]),
                Change::Remove(3),
                Change::Add(vec![object(4, 7, 400)]),
            ],
        )
        .unwrap();
        let undo = t.revert(1);
        t.apply(2, undo).unwrap();
        assert_eq!(ids(&t.head()), ids(&t.at(0)), "as it was before change 1");
        assert_eq!(
            ids(&t.at(1)),
            [(1, 100), (2, 250), (4, 400)],
            "change 1 is still in the past"
        );
    }

    #[test]
    fn the_head_is_a_plain_tile_and_the_history_is_beside_it() {
        let mut t = start();
        t.apply(
            1,
            vec![
                Change::Replace(2, vec![object(2, 3, 250)]),
                Change::Remove(3),
            ],
        )
        .unwrap();
        t.apply(2, vec![Change::Add(vec![object(4, 7, 400)])])
            .unwrap();
        let (head, history) = (t.encode_head(), t.encode_history());
        // A viewer reads the head as it reads any tile, and sees the present.
        assert_eq!(ids(&Tile::decode(&head).unwrap()), ids(&t.head()));
        // With the sidecar, every moment comes back.
        let back = VersionedTile::decode(&head, Some(&history)).unwrap();
        for changeset in 0..=3 {
            assert_eq!(
                ids(&back.at(changeset)),
                ids(&t.at(changeset)),
                "changeset {changeset}"
            );
        }
        assert_eq!(back.versions.len(), t.versions.len());
        // Without it the head alone is a map with no past.
        let alone = VersionedTile::decode(&head, None).unwrap();
        assert_eq!(ids(&alone.head()), ids(&t.head()));
        assert_eq!(alone.versions.len(), 3);
    }

    #[test]
    fn a_map_with_no_changes_has_a_tiny_sidecar() {
        let t = start();
        let history = t.encode_history();
        assert!(history.len() < 16, "{} bytes", history.len());
        let back = VersionedTile::decode(&t.encode_head(), Some(&history)).unwrap();
        assert_eq!(ids(&back.head()), ids(&t.head()));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut t = start();
        t.apply(1, vec![Change::Remove(1)]).unwrap();
        let (head, history) = (t.encode_head(), t.encode_history());
        for cut in 0..history.len() {
            let _ = VersionedTile::decode(&head, Some(&history[..cut]));
        }
        let mut bad = history.clone();
        for i in 0..bad.len() {
            bad[i] ^= 0xa5;
            let _ = VersionedTile::decode(&head, Some(&bad));
            bad[i] ^= 0xa5;
        }
        let log = ChangeLog {
            changesets: vec![Changeset {
                id: 1,
                time: 5,
                author: 2,
                comment: "x".into(),
                tool: "y".into(),
            }],
        }
        .encode();
        for cut in 0..log.len() {
            let _ = ChangeLog::decode(&log[..cut]);
        }
        assert!(ChangeLog::decode(&[]).is_err());
    }

    #[test]
    fn the_log_says_who_when_and_why_and_finds_the_map_at_a_time() {
        let log = ChangeLog {
            changesets: vec![
                Changeset {
                    id: 1,
                    time: 1_700_000_000,
                    author: 7,
                    comment: "first survey".into(),
                    tool: "Mapper".into(),
                },
                Changeset {
                    id: 2,
                    time: 1_700_086_400,
                    author: 9,
                    comment: "ha cambiato la strada — é".into(),
                    tool: "omap_to_tiles".into(),
                },
                Changeset {
                    id: 3,
                    time: 1_700_090_000,
                    author: 7,
                    comment: String::new(),
                    tool: String::new(),
                },
            ],
        };
        let back = ChangeLog::decode(&log.encode()).unwrap();
        assert_eq!(back, log);
        assert_eq!(back.get(2).unwrap().author, 9);
        assert!(back.get(9).is_none());
        assert_eq!(back.at_time(1_699_999_999), None, "before the first");
        assert_eq!(back.at_time(1_700_000_000), Some(1));
        assert_eq!(back.at_time(1_700_089_999), Some(2));
        assert_eq!(back.at_time(i64::MAX), Some(3));
    }
}
