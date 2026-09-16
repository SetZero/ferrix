//! zsh's hash tables (`hashtable.c`): the parameter, function, alias and
//! option tables, and every association.
//!
//! The order a table is scanned in is visible — `${(k)hash}`, `$options`,
//! `functions` without `-o` — so this keeps zsh's layout exactly: the same
//! hash function over the metafied name, a new node at the head of its chain,
//! and the table growing fourfold, re-added bucket by bucket, once it holds
//! twice as many nodes as buckets.

/// zsh's `hasher`.
pub(crate) fn hasher(name: &[u8]) -> u32 {
    let mut hashval: u32 = 0;
    for &c in name {
        hashval = hashval.wrapping_add((hashval << 5).wrapping_add(u32::from(c)));
    }
    hashval
}

/// A table keyed by metafied names.
#[derive(Debug, Clone)]
pub(crate) struct HashTable<V> {
    /// Each bucket's chain, head first.
    nodes: Vec<Vec<(Vec<u8>, V)>>,
    ct: usize,
}

impl<V> Default for HashTable<V> {
    fn default() -> Self {
        HashTable::new(17)
    }
}

impl<V> HashTable<V> {
    /// zsh's `newhashtable(size)`. A size of 0, which zsh uses for an empty
    /// association, is taken as 1 so that hashing has somewhere to go.
    pub(crate) fn new(size: usize) -> Self {
        let size = size.max(1);
        let mut nodes = Vec::with_capacity(size);
        nodes.resize_with(size, Vec::new);
        HashTable { nodes, ct: 0 }
    }

    fn bucket(&self, name: &[u8]) -> usize {
        usize::try_from(hasher(name)).unwrap_or(0) % self.nodes.len().max(1)
    }

    /// The number of nodes.
    pub(crate) fn len(&self) -> usize {
        self.ct
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ct == 0
    }

    /// zsh's `addhashnode2`: replace a node of the same name where it stands,
    /// or put a new one at the head of its chain. Returns the old value.
    pub(crate) fn insert(&mut self, name: Vec<u8>, value: V) -> Option<V> {
        let b = self.bucket(&name);
        let chain = self.nodes.get_mut(b)?;
        if let Some(slot) = chain.iter_mut().find(|(n, _)| *n == name) {
            return Some(std::mem::replace(&mut slot.1, value));
        }
        chain.insert(0, (name, value));
        self.ct += 1;
        if self.ct >= self.nodes.len() * 2 {
            self.expand();
        }
        None
    }

    fn expand(&mut self) {
        let osize = self.nodes.len();
        let old = std::mem::take(&mut self.nodes);
        self.nodes.resize_with(osize * 4, Vec::new);
        self.ct = 0;
        for chain in old {
            for (name, value) in chain {
                let b = self.bucket(&name);
                if let Some(c) = self.nodes.get_mut(b) {
                    c.insert(0, (name, value));
                    self.ct += 1;
                }
            }
        }
    }

    pub(crate) fn get(&self, name: &[u8]) -> Option<&V> {
        self.nodes
            .get(self.bucket(name))?
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    pub(crate) fn get_mut(&mut self, name: &[u8]) -> Option<&mut V> {
        let b = self.bucket(name);
        self.nodes
            .get_mut(b)?
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    pub(crate) fn contains(&self, name: &[u8]) -> bool {
        self.get(name).is_some()
    }

    /// zsh's `removehashnode`.
    pub(crate) fn remove(&mut self, name: &[u8]) -> Option<V> {
        let b = self.bucket(name);
        let chain = self.nodes.get_mut(b)?;
        let pos = chain.iter().position(|(n, _)| n == name)?;
        self.ct -= 1;
        Some(chain.remove(pos).1)
    }

    /// Every node in scan order: bucket by bucket, each chain head first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &V)> {
        self.nodes.iter().flatten().map(|(n, v)| (n, v))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (&Vec<u8>, &mut V)> {
        self.nodes.iter_mut().flatten().map(|(n, v)| (&*n, v))
    }

    /// The names in scan order.
    pub(crate) fn keys(&self) -> Vec<Vec<u8>> {
        self.iter().map(|(n, _)| n.clone()).collect()
    }

    /// The names sorted as `scanhashtable(..., sorted)` sorts them.
    pub(crate) fn sorted_keys(&self) -> Vec<Vec<u8>> {
        let mut k = self.keys();
        k.sort_by(|a, b| crate::utils::ztrcmp(a, b));
        k
    }

    /// zsh's `emptyhashtable`/`resizehashtable`.
    pub(crate) fn clear(&mut self) {
        for c in &mut self.nodes {
            c.clear();
        }
        self.ct = 0;
    }

    /// Keep only the nodes `f` accepts.
    pub(crate) fn retain(&mut self, mut f: impl FnMut(&[u8], &mut V) -> bool) {
        let mut removed = 0;
        for c in &mut self.nodes {
            let before = c.len();
            c.retain_mut(|(n, v)| f(n, v));
            removed += before - c.len();
        }
        self.ct -= removed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_zsh() {
        assert_eq!(hasher(b""), 0);
        assert_eq!(hasher(b"a"), 97);
        assert_eq!(hasher(b"ab"), 97 + (97 << 5) + 98);
    }

    #[test]
    fn grows_fourfold_and_keeps_every_node() {
        let mut t = HashTable::new(1);
        for i in 0..100u32 {
            let _ = t.insert(i.to_string().into_bytes(), i);
        }
        assert_eq!(t.len(), 100);
        for i in 0..100u32 {
            assert_eq!(t.get(i.to_string().as_bytes()), Some(&i));
        }
        assert_eq!(t.remove(b"5"), Some(5));
        assert_eq!(t.len(), 99);
    }
}
