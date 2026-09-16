//! Parameters that zsh's loadable modules provide (`zsh/parameter`,
//! `zsh/datetime`, `zsh/terminfo`, ...). Each module's parameters become
//! variants here as the module is ported; until then there are none, and
//! the dispatch in `params.rs` has nothing to reach.

use crate::params::{Param, PmRef};
use crate::shell::Shell;

/// Which module parameter a `Gsu::Module` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModParam {}

impl Shell {
    pub(crate) fn module_getsfn(&mut self, m: ModParam, _r: &PmRef, _name: &[u8]) -> Vec<u8> {
        match m {}
    }

    pub(crate) fn module_setsfn(
        &mut self,
        m: ModParam,
        _r: &mut PmRef,
        _name: &[u8],
        _x: Option<Vec<u8>>,
    ) {
        match m {}
    }

    pub(crate) fn module_getifn(&mut self, m: ModParam, _r: &PmRef, _name: &[u8]) -> i64 {
        match m {}
    }

    pub(crate) fn module_setifn(&mut self, m: ModParam, _r: &mut PmRef, _name: &[u8], _x: i64) {
        match m {}
    }

    pub(crate) fn module_getffn(&mut self, m: ModParam, _r: &PmRef, _name: &[u8]) -> f64 {
        match m {}
    }

    pub(crate) fn module_getafn(&mut self, m: ModParam, _r: &PmRef, _name: &[u8]) -> Vec<Vec<u8>> {
        match m {}
    }

    pub(crate) fn module_setafn(
        &mut self,
        m: ModParam,
        _r: &mut PmRef,
        _name: &[u8],
        _x: Option<Vec<Vec<u8>>>,
    ) {
        match m {}
    }

    pub(crate) fn module_sethfn(
        &mut self,
        m: ModParam,
        _r: &mut PmRef,
        _name: &[u8],
        _x: Option<crate::hashtable::HashTable<Param>>,
    ) {
        match m {}
    }

    pub(crate) fn module_unsetfn(&mut self, m: ModParam, _r: &mut PmRef, _name: &[u8], _exp: bool) {
        match m {}
    }

    pub(crate) fn module_scan_hash(&mut self, m: ModParam, _r: &PmRef) -> Vec<(Vec<u8>, PmRef)> {
        match m {}
    }

    pub(crate) fn module_hash_getnode(
        &mut self,
        m: ModParam,
        _r: &PmRef,
        _key: &[u8],
    ) -> Option<PmRef> {
        match m {}
    }

    /// zsh's autoload of a module parameter: none are autoloadable yet.
    pub(crate) fn autoload_module_param(&mut self, _name: &[u8]) {}
}
