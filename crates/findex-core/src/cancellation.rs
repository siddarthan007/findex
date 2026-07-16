//! Low-overhead cooperative cancellation for CPU-bound work.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("operation cancelled")]
pub struct Cancelled;

thread_local! {
    static CURRENT: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}

/// Install a cancellation token for the duration of one synchronous task.
pub fn with_token<T>(token: CancellationToken, action: impl FnOnce() -> T) -> T {
    CURRENT.with(|current| {
        let previous = current.replace(Some(token));
        let result = action();
        current.replace(previous);
        result
    })
}

/// Copy the current task token into a Rayon worker or another child thread.
pub fn inherited_token() -> Option<CancellationToken> {
    CURRENT.with(|current| current.borrow().clone())
}

pub fn is_cancelled() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    })
}

pub fn checkpoint() -> Result<(), Cancelled> {
    if is_cancelled() {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

pub fn checkpoint_token(token: Option<&CancellationToken>) -> Result<(), Cancelled> {
    if token.is_some_and(CancellationToken::is_cancelled) {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_token_restores_previous_context() {
        let token = CancellationToken::default();
        assert!(!is_cancelled());
        with_token(token.clone(), || {
            token.cancel();
            assert!(checkpoint().is_err());
        });
        assert!(!is_cancelled());
    }
}
