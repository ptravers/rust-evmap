//! A lock-free, eventually consistent, concurrent multi-value map.
//!
//! This map implementation allows reads and writes to execute entirely in parallel, with no
//! implicit synchronization overhead. Reads never take locks on their critical path, and neither
//! do writes assuming there is a single writer (multi-writer is possible using a `Mutex`), which
//! significantly improves performance under contention.
//!
//! The trade-off exposed by this module is one of eventual consistency: writes are not visible to
//! readers except following explicit synchronization. Specifically, readers only see the
//! operations that preceeded the last call to `WriteHandle::refresh` by a writer. This lets
//! writers decide how stale they are willing to let reads get. They can refresh the map after
//! every write to emulate a regular concurrent `HashMap`, or they can refresh only occasionally to
//! reduce the synchronization overhead at the cost of stale reads.
//!
//! For read-heavy workloads, the scheme used by this module is particularly useful. Writers can
//! afford to refresh after every write, which provides up-to-date reads, and readers remain fast
//! as they do not need to ever take locks.
//!
//! The map is multi-value, meaning that every key maps to a *collection* of values. This
//! introduces some memory cost by adding a layer of indirection through a `Vec` for each value,
//! but enables more advanced use. This choice was made as it would not be possible to emulate such
//! functionality on top of the semantics of this map (think about it -- what would the operational
//! log contain?).
//!
//! To faciliate more advanced use-cases, each of the two maps also carry some customizeable
//! meta-information. The writers may update this at will, and when a refresh happens, the current
//! meta will also be made visible to readers. This could be useful, for example, to indicate what
//! time the refresh happened.
//!
//! # Examples
//!
//! Single-reader, single-writer
//!
//! ```
//! // new will use the default HashMap hasher, and a meta of ()
//! // note that we get separate read and write handles
//! // the read handle can be cloned to have more readers
//! let (book_reviews_r, mut book_reviews_w) = evmap::new();
//!
//! // review some books.
//! book_reviews_w.insert("Adventures of Huckleberry Finn",    "My favorite book.");
//! book_reviews_w.insert("Grimms' Fairy Tales",               "Masterpiece.");
//! book_reviews_w.insert("Pride and Prejudice",               "Very enjoyable.");
//! book_reviews_w.insert("The Adventures of Sherlock Holmes", "Eye lyked it alot.");
//!
//! // at this point, reads from book_reviews_r will not see any of the reviews!
//! assert_eq!(book_reviews_r.len(), 0);
//! // we need to refresh first to make the writes visible
//! book_reviews_w.refresh();
//! assert_eq!(book_reviews_r.len(), 4);
//! // reads will now return Some() because the map has been initialized
//! assert_eq!(book_reviews_r.get("Grimms' Fairy Tales").map(|rs| rs.len()), Some(1));
//!
//! // remember, this is a multi-value map, so we can have many reviews
//! book_reviews_w.insert("Grimms' Fairy Tales",               "Eh, the title seemed weird.");
//! book_reviews_w.insert("Pride and Prejudice",               "Too many words.");
//!
//! // but again, new writes are not yet visible
//! assert_eq!(book_reviews_r.get("Grimms' Fairy Tales").map(|rs| rs.len()), Some(1));
//!
//! // we need to refresh first
//! book_reviews_w.refresh();
//! assert_eq!(book_reviews_r.get("Grimms' Fairy Tales").map(|rs| rs.len()), Some(2));
//!
//! // oops, this review has a lot of spelling mistakes, let's delete it.
//! // empty deletes *all* reviews (though in this case, just one)
//! book_reviews_w.empty("The Adventures of Sherlock Holmes");
//! // but again, it's not visible to readers until we refresh
//! assert_eq!(book_reviews_r.get("The Adventures of Sherlock Holmes").map(|rs| rs.len()), Some(1));
//! book_reviews_w.refresh();
//! assert_eq!(book_reviews_r.get("The Adventures of Sherlock Holmes").map(|rs| rs.len()), None);
//!
//! // look up the values associated with some keys.
//! let to_find = ["Pride and Prejudice", "Alice's Adventure in Wonderland"];
//! for book in &to_find {
//!     if let Some(reviews) = book_reviews_r.get(book) {
//!         for review in &*reviews {
//!             println!("{}: {}", book, review);
//!         }
//!     } else {
//!         println!("{} is unreviewed.", book);
//!     }
//! }
//!
//! // iterate over everything.
//! for (book, reviews) in &book_reviews_r.read() {
//!     for review in reviews {
//!         println!("{}: \"{}\"", book, review);
//!     }
//! }
//! ```
//!
//! Reads from multiple threads are possible by cloning the `ReadHandle`.
//!
//! ```
//! use std::thread;
//! let (book_reviews_r, mut book_reviews_w) = evmap::new();
//!
//! // start some readers
//! let readers: Vec<_> = (0..4).map(|_| {
//!     let r = book_reviews_r.clone();
//!     thread::spawn(move || {
//!         loop {
//!             let l = r.len();
//!             if l == 0 {
//!                 thread::yield_now();
//!             } else {
//!                 // the reader will either see all the reviews,
//!                 // or none of them, since refresh() is atomic.
//!                 assert_eq!(l, 4);
//!                 break;
//!             }
//!         }
//!     })
//! }).collect();
//!
//! // do some writes
//! book_reviews_w.insert("Adventures of Huckleberry Finn",    "My favorite book.");
//! book_reviews_w.insert("Grimms' Fairy Tales",               "Masterpiece.");
//! book_reviews_w.insert("Pride and Prejudice",               "Very enjoyable.");
//! book_reviews_w.insert("The Adventures of Sherlock Holmes", "Eye lyked it alot.");
//! // expose the writes
//! book_reviews_w.refresh();
//!
//! // you can read through the write handle
//! assert_eq!(book_reviews_w.len(), 4);
//!
//! // the original read handle still works too
//! assert_eq!(book_reviews_r.len(), 4);
//!
//! // all the threads should eventually see .len() == 4
//! for r in readers.into_iter() {
//!     assert!(r.join().is_ok());
//! }
//! ```
//!
//! If multiple writers are needed, the `WriteHandle` must be protected by a `Mutex`.
//!
//! ```
//! use std::thread;
//! use std::sync::{Arc, Mutex};
//! let (book_reviews_r, mut book_reviews_w) = evmap::new();
//!
//! // start some writers.
//! // since evmap does not support concurrent writes, we need
//! // to protect the write handle by a mutex.
//! let w = Arc::new(Mutex::new(book_reviews_w));
//! let writers: Vec<_> = (0..4).map(|i| {
//!     let w = w.clone();
//!     thread::spawn(move || {
//!         let mut w = w.lock().unwrap();
//!         w.insert(i, true);
//!         w.refresh();
//!     })
//! }).collect();
//!
//! // eventually we should see all the writes
//! while book_reviews_r.len() < 4 { thread::yield_now(); };
//!
//! // all the threads should eventually finish writing
//! for w in writers.into_iter() {
//!     assert!(w.join().is_ok());
//! }
//! ```
//!
//! # Implementation
//!
//! Under the hood, the map is implemented using two regular `HashMap`s, an operational log,
//! epoch counting, and some pointer magic. There is a single pointer through which all readers
//! go. It points to a `HashMap`, which the readers access in order to read data. Every time a read
//! has accessed the pointer, they increment a local epoch counter, and they update it again when
//! they have finished the read (see #3 for more information). When a write occurs, the writer
//! updates the other `HashMap` (for which there are no readers), and also stores a copy of the
//! change in a log (hence the need for `Clone` on the keys and values). When
//! `WriteHandle::refresh` is called, the writer, atomically swaps the reader pointer to point to
//! the other map. It then waits for the epochs of all current readers to change, and then replays
//! the operational log to bring the stale map up to date.
//!
//! Since the implementation uses regular `HashMap`s under the hood, table resizing is fully
//! supported. It does, however, also mean that the memory usage of this implementation is
//! approximately twice of that of a regular `HashMap`, and more if writes rarely refresh after
//! writing.
//!
//! # Small Vector Optimization
//!
//! By default, the value-set for each key in the map uses the `smallvec` crate to keep a
//! maximum of one element stored inline with the map, as opposed to separately heap-allocated
//! with a plain `Vec`. Operations such as `Fit` and `Replace` will automatically switch
//! back to the inline storage if possible. This is ideal for maps that mostly use one
//! element per key, as it can improvate memory locality with less indirection.
//!
//! If this is undesirable, simple set:
//!
//! ```toml
//! default-features = false
//! ```
//!
//! in the `evmap` dependency entry, and `Vec` will always be used internally.
#![deny(missing_docs)]

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash};
use std::sync::{atomic, Arc, Mutex};

mod inner;
use crate::inner::Inner;

pub(crate) type Epochs = Arc<Mutex<Vec<Arc<atomic::AtomicUsize>>>>;

/// Unary predicate used to retain elements.
///
/// The arguments to the predicate function are the current value in the value-set, and `true` if
/// this is the first value in the value-set on the second map, or `false` otherwise.
pub struct Predicate<V>(pub(crate) Box<dyn FnMut(&V, bool) -> bool + Send + Sync>);

impl<V> Predicate<V> {
    /// Evaluate the predicate for the given element
    #[inline]
    pub fn eval(&mut self, value: &V, reset: bool) -> bool {
        (*self.0)(value, reset)
    }
}

impl<V> PartialEq for Predicate<V> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        &*self.0 as *const _ == &*other.0 as *const _
    }
}

impl<V> Eq for Predicate<V> {}

impl<V> fmt::Debug for Predicate<V> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Predicate")
            .field(&format_args!("{:p}", &*self.0 as *const _))
            .finish()
    }
}

/// A pending map operation.
///
/// Note that this enum should be considered
/// [non-exhaustive](https://github.com/rust-lang/rust/issues/44109).
#[derive(PartialEq, Eq, Debug)]
// TODO: #[non_exhaustive]
// https://github.com/rust-lang/rust/issues/44109
pub enum Operation<K, V> {
    /// Replace the set of entries for this key with this value.
    Replace(K, V),
    /// Add this value to the set of entries for this key.
    Add(K, V),
    /// Remove this value from the set of entries for this key.
    Remove(K, V),
    /// Remove the value set for this key.
    Empty(K),
    #[cfg(feature = "indexed")]
    /// Drop a key at a random index
    EmptyRandom(usize),
    /// Remove all values in the value set for this key.
    Clear(K),
    /// Remove all values for all keys.
    ///
    /// Note that this will iterate once over all the keys internally.
    Purge,
    /// Retains all values matching the given predicate.
    Retain(K, Predicate<V>),
    /// Shrinks a value-set to it's minimum necessary size, freeing memory
    /// and potentially improving cache locality if the `smallvec` feature is used.
    ///
    /// If no key is given, all value-sets will shrink to fit.
    Fit(Option<K>),
    /// Reserves capacity for some number of additional elements in a value-set,
    /// or creates an empty value-set for this key with the given capacity if
    /// it doesn't already exist.
    ///
    /// This can improve performance by pre-allocating space for large value-sets.
    Reserve(K, usize),
    // Since we have a feature that adds an enum variant, features are only additive (as they need
    // to be) if users never try to exhaustively match on this enum. Once rust-lang/rust#44109
    // lands, we'll have a more standard way to do this, but for now we rely on this trick:
    #[doc(hidden)]
    __Nonexhaustive,
}

mod write;
pub use crate::write::WriteHandle;

mod read;
pub use crate::read::{MapReadRef, ReadGuard, ReadGuardIter, ReadHandle, ReadHandleFactory};

pub mod shallow_copy;
pub use crate::shallow_copy::ShallowCopy;

/// Options for how to initialize the map.
///
/// In particular, the options dictate the hashing function, meta type, and initial capacity of the
/// map.
pub struct Options<M, S>
where
    S: BuildHasher,
{
    meta: M,
    hasher: S,
    capacity: Option<usize>,
}

impl Default for Options<(), RandomState> {
    fn default() -> Self {
        Options {
            meta: (),
            hasher: RandomState::default(),
            capacity: None,
        }
    }
}

impl<M, S> Options<M, S>
where
    S: BuildHasher,
{
    /// Set the initial meta value for the map.
    pub fn with_meta<M2>(self, meta: M2) -> Options<M2, S> {
        Options {
            meta,
            hasher: self.hasher,
            capacity: self.capacity,
        }
    }

    /// Set the hasher used for the map.
    pub fn with_hasher<S2>(self, hash_builder: S2) -> Options<M, S2>
    where
        S2: BuildHasher,
    {
        Options {
            meta: self.meta,
            hasher: hash_builder,
            capacity: self.capacity,
        }
    }

    /// Set the initial capacity for the map.
    pub fn with_capacity(self, capacity: usize) -> Options<M, S> {
        Options {
            meta: self.meta,
            hasher: self.hasher,
            capacity: Some(capacity),
        }
    }

    /// Create the map, and construct the read and write handles used to access it.
    #[allow(clippy::type_complexity)]
    pub fn construct<K, V>(self) -> (ReadHandle<K, V, M, S>, WriteHandle<K, V, M, S>)
    where
        K: Eq + Hash + Clone,
        S: BuildHasher + Clone,
        V: Eq + ShallowCopy,
        M: 'static + Clone,
    {
        let epochs = Default::default();
        let inner = if let Some(cap) = self.capacity {
            Inner::with_capacity_and_hasher(self.meta, cap, self.hasher)
        } else {
            Inner::with_hasher(self.meta, self.hasher)
        };

        let mut w_handle = inner.clone();
        w_handle.mark_ready();
        let r = read::new(inner, Arc::clone(&epochs));
        let w = write::new(w_handle, epochs, r.clone());
        (r, w)
    }
}

/// Create an empty eventually consistent map.
///
/// Use the [`Options`](./struct.Options.html) builder for more control over initialization.
#[allow(clippy::type_complexity)]
pub fn new<K, V>() -> (
    ReadHandle<K, V, (), RandomState>,
    WriteHandle<K, V, (), RandomState>,
)
where
    K: Eq + Hash + Clone,
    V: Eq + ShallowCopy,
{
    Options::default().construct()
}

/// Create an empty eventually consistent map with meta information.
///
/// Use the [`Options`](./struct.Options.html) builder for more control over initialization.
#[allow(clippy::type_complexity)]
pub fn with_meta<K, V, M>(
    meta: M,
) -> (
    ReadHandle<K, V, M, RandomState>,
    WriteHandle<K, V, M, RandomState>,
)
where
    K: Eq + Hash + Clone,
    V: Eq + ShallowCopy,
    M: 'static + Clone,
{
    Options::default().with_meta(meta).construct()
}

/// Create an empty eventually consistent map with meta information and custom hasher.
///
/// Use the [`Options`](./struct.Options.html) builder for more control over initialization.
#[allow(clippy::type_complexity)]
pub fn with_hasher<K, V, M, S>(
    meta: M,
    hasher: S,
) -> (ReadHandle<K, V, M, S>, WriteHandle<K, V, M, S>)
where
    K: Eq + Hash + Clone,
    V: Eq + ShallowCopy,
    M: 'static + Clone,
    S: BuildHasher + Clone,
{
    Options::default()
        .with_hasher(hasher)
        .with_meta(meta)
        .construct()
}

// test that ReadHandle isn't Sync
// waiting on https://github.com/rust-lang/rust/issues/17606
//#[test]
//fn is_not_sync() {
//    use std::sync;
//    use std::thread;
//    let (r, mut w) = new();
//    w.insert(true, false);
//    let x = sync::Arc::new(r);
//    thread::spawn(move || { drop(x); });
//}
