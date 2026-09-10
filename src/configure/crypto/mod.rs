//! Making the target's storage secret: LUKS containers and the keys that
//! unlock them, dm-verity sealing, and SecureBoot signing.

pub mod encryption;
pub mod keyfiles;
pub mod secureboot;
pub mod verity;
