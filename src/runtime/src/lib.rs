//! Contains the runtime and core library for the Koto language

mod error;
mod external;
mod file;
mod frame;
mod meta_map;
mod stdio;
mod value_iterator;
mod value_key;
mod value_list;
mod value_map;
mod value_number;
mod value_sort;
mod value_string;
mod value_tuple;
mod vm;

pub mod core;
pub mod num2;
pub mod num4;
pub mod value;

pub use {
    error::*,
    external::{ExternalData, ExternalFunction, ExternalValue},
    file::{KotoFile, KotoRead, KotoWrite},
    koto_bytecode::{CompilerError, Loader, LoaderError},
    koto_parser::ParserError,
    meta_map::{BinaryOp, MetaKey, MetaMap, UnaryOp},
    num2::Num2,
    num4::Num4,
    parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
    stdio::{DefaultStderr, DefaultStdin, DefaultStdout},
    value::{FunctionInfo, Value},
    value_iterator::{ExternalIterator, IntRange, ValueIterator, ValueIteratorOutput},
    value_key::ValueKey,
    value_list::{ValueList, ValueVec},
    value_map::{DataMap, ValueMap},
    value_number::ValueNumber,
    value_string::ValueString,
    value_tuple::ValueTuple,
    vm::{CallArgs, Vm, VmSettings},
};
