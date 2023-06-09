use std::sync::PoisonError;
use thiserror::Error;

/// Channel Error Definitions
#[derive(Debug, Error, Clone, PartialEq, Eq, Copy)]
#[non_exhaustive]
pub enum ChannelError {
    /// NoReceiverLeft will generate when a sender try to send a message to a channel without any receiver left.
    #[error("No Receiver Left!")]
    NoReceiverLeft,

    /// NoSenderLeft will generate when a receiver try to receive a message from a channel without any sender left.
    #[error("No Sender Left!")]
    NoSenderLeft,

    /// If another user of this mutex panicked while holding the mutex, then this call will return an error once the mutex is acquired.
    #[error("Poisoned Lock")]
    PoisonedLock,
}

impl<T> From<PoisonError<T>> for ChannelError {
    #[inline]
    fn from(_err: PoisonError<T>) -> Self {
        ChannelError::PoisonedLock
    }
}
