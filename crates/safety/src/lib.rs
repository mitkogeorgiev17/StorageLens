//! Safety-critical deletion machinery: path guards, Trash, manifests, undo.
//!
//! Every public function here is expected to be covered by a test. See issue #35.
//!
//! Filled in by issues #29–#30.

#![forbid(unsafe_code)]
