use crate::uany::{UnsafeAny, UnsafeAnyExt};
use std::any::Any;
use std::fmt::Debug;

/// A marker trait meant for use as the `A` parameter in `TypeMap`.
///
/// This can be used to construct `TypeMap`s containing only types which
/// implement `Debug` like so: `TypeMap::<DebugAny>::custom()`, which produces
/// a `TypeMap<DebugAny>`. Combine `DebugAny` with `Send` or `Sync` to add
/// additional bounds.
///
/// There is also an exported alias for this type of `TypeMap`, `DebugMap`.
pub trait DebugAny: Any + Debug {}
impl<T: Any + Debug> DebugAny for T {}

unsafe impl UnsafeAnyExt for dyn DebugAny {}
unsafe impl UnsafeAnyExt for dyn DebugAny + Send {}
unsafe impl UnsafeAnyExt for dyn DebugAny + Sync {}
unsafe impl UnsafeAnyExt for dyn DebugAny + Send + Sync {}

/// A marker trait meant for use as the `A` parameter in `TypeMap`.
///
/// This can be used to construct `TypeMap`s containing only types which
/// implement `Clone` like so: `TypeMap::<CloneAny>::custom()`, which produces
/// a `TypeMap<CloneAny>`. Combine `CloneAny` with `Send` or `Sync` to add
/// additional bounds.
///
/// There is also an exported alias for this type of `TypeMap`, `CloneAny`.
pub trait CloneAny: Any {
    #[doc(hidden)]
    fn clone_any(&self) -> Box<dyn CloneAny>;
    #[doc(hidden)]
    fn clone_any_send(&self) -> Box<dyn CloneAny + Send>
    where
        Self: Send;
    #[doc(hidden)]
    fn clone_any_sync(&self) -> Box<dyn CloneAny + Sync>
    where
        Self: Sync;
    #[doc(hidden)]
    fn clone_any_send_sync(&self) -> Box<dyn CloneAny + Send + Sync>
    where
        Self: Send + Sync;
}

impl<T: Any + Clone> CloneAny for T {
    fn clone_any(&self) -> Box<dyn CloneAny> {
        Box::new(self.clone())
    }

    fn clone_any_send(&self) -> Box<dyn CloneAny + Send>
    where
        Self: Send,
    {
        Box::new(self.clone())
    }

    fn clone_any_sync(&self) -> Box<dyn CloneAny + Sync>
    where
        Self: Sync,
    {
        Box::new(self.clone())
    }

    fn clone_any_send_sync(&self) -> Box<dyn CloneAny + Send + Sync>
    where
        Self: Send + Sync,
    {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn CloneAny> {
    fn clone(&self) -> Self {
        (**self).clone_any()
    }
}

impl Clone for Box<dyn CloneAny + Send> {
    fn clone(&self) -> Self {
        (**self).clone_any_send()
    }
}

impl Clone for Box<dyn CloneAny + Sync> {
    fn clone(&self) -> Self {
        (**self).clone_any_sync()
    }
}

impl Clone for Box<dyn CloneAny + Send + Sync> {
    fn clone(&self) -> Self {
        (**self).clone_any_send_sync()
    }
}

unsafe impl UnsafeAnyExt for dyn CloneAny {}
unsafe impl UnsafeAnyExt for dyn CloneAny + Send {}
unsafe impl UnsafeAnyExt for dyn CloneAny + Sync {}
unsafe impl UnsafeAnyExt for dyn CloneAny + Send + Sync {}

#[doc(hidden)] // Not actually exported
pub unsafe trait Implements<A: ?Sized + UnsafeAnyExt> {
    fn into_object(self) -> Box<A>;
}

unsafe impl<T: UnsafeAny> Implements<dyn UnsafeAny> for T {
    fn into_object(self) -> Box<dyn UnsafeAny> {
        Box::new(self)
    }
}

unsafe impl<T: UnsafeAny + Send> Implements<dyn UnsafeAny + Send> for T {
    fn into_object(self) -> Box<dyn UnsafeAny + Send> {
        Box::new(self)
    }
}

unsafe impl<T: UnsafeAny + Sync> Implements<dyn UnsafeAny + Sync> for T {
    fn into_object(self) -> Box<dyn UnsafeAny + Sync> {
        Box::new(self)
    }
}

unsafe impl<T: UnsafeAny + Send + Sync> Implements<dyn UnsafeAny + Send + Sync> for T {
    fn into_object(self) -> Box<dyn UnsafeAny + Send + Sync> {
        Box::new(self)
    }
}

unsafe impl<T: CloneAny> Implements<dyn CloneAny> for T {
    fn into_object(self) -> Box<dyn CloneAny> {
        Box::new(self)
    }
}

unsafe impl<T: CloneAny + Send> Implements<dyn CloneAny + Send> for T {
    fn into_object(self) -> Box<dyn CloneAny + Send> {
        Box::new(self)
    }
}

unsafe impl<T: CloneAny + Send + Sync> Implements<dyn CloneAny + Send + Sync> for T {
    fn into_object(self) -> Box<dyn CloneAny + Send + Sync> {
        Box::new(self)
    }
}

unsafe impl<T: DebugAny> Implements<dyn DebugAny> for T {
    fn into_object(self) -> Box<dyn DebugAny> {
        Box::new(self)
    }
}

unsafe impl<T: DebugAny + Send> Implements<dyn DebugAny + Send> for T {
    fn into_object(self) -> Box<dyn DebugAny + Send> {
        Box::new(self)
    }
}

unsafe impl<T: DebugAny + Sync> Implements<dyn DebugAny + Sync> for T {
    fn into_object(self) -> Box<dyn DebugAny + Sync> {
        Box::new(self)
    }
}

unsafe impl<T: DebugAny + Send + Sync> Implements<dyn DebugAny + Send + Sync> for T {
    fn into_object(self) -> Box<dyn DebugAny + Send + Sync> {
        Box::new(self)
    }
}
