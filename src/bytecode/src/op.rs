/// The operation identifiers used in Koto bytecode
///
/// See [InstructionReader]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Op {
    /// Copies a register to another
    ///
    /// bytes: [target, source]
    Copy,

    /// Sets a register to contain Empty
    ///
    /// bytes: [target]
    SetEmpty,

    /// Sets a register to contain Bool(false)
    ///
    /// bytes: [target]
    SetFalse,

    /// Sets a register to contain Bool(true)
    ///
    /// bytes: [target]
    SetTrue,

    /// Sets a register to contain Int(0)
    ///
    /// bytes: [target]
    Set0,

    /// Sets a register to contain Int(1)
    ///
    /// bytes: [target]
    Set1,

    /// Sets a register to contain u8(n)
    ///
    /// bytes: [target, n]
    SetNumberU8,

    /// Loads an f64 constant into a register
    ///
    /// bytes: [target, constant]
    LoadFloat,

    /// Loads an f64 constant with a u16 index into a register
    ///
    /// bytes: [target, constant[2]]
    LoadFloat16,

    /// Loads an f64 constant with a u24 index into a register
    ///
    /// bytes: [target, constant[3]]
    LoadFloat24,

    /// Loads an i64 constant into a register
    ///
    /// bytes: [target, constant]
    LoadInt,

    /// Loads an i64 constant with a u16 index into a register
    ///
    /// bytes: [target, constant[2]]
    LoadInt16,

    /// Loads an i64 constant with a u24 index into a register
    ///
    /// bytes: [target, constant[3]]
    LoadInt24,

    /// Loads a string constant into a register
    ///
    /// bytes: [target, constant]
    LoadString,

    /// Loads a string constant with a u16 index into a register
    ///
    /// bytes: [target, constant[2]]
    LoadString16,

    /// Loads a string constant with a u24 index into a register
    ///
    /// bytes: [target, constant[3]]
    LoadString24,

    /// Loads a non-local value into a register
    ///
    /// bytes: [target, constant]
    LoadNonLocal,

    /// Loads a non-local value with a u16 id index into a register
    ///
    /// bytes: [target, constant[2]]
    LoadNonLocal16,

    /// Loads a non-local value with a u24 id index into a register
    ///
    /// bytes: [target, constant[3]]
    LoadNonLocal24,

    /// Imports a value
    ///
    /// The name of the value to be imported will be placed in the register before running this op,
    /// the imported value will then be placed in the same register.
    ///
    /// bytes: [register]
    Import,

    MakeTempTuple,    // register, start register, count
    MakeMap,          // register, size hint
    MakeMap32,        // register, size hint[4]
    MakeNum2,         // register, element count, first element
    MakeNum4,         // register, element count, first element
    MakeIterator,     // register, range
    SequenceStart,    // register, size hint
    SequenceStart32,  // register, size hint[4]
    SequencePush,     // register, value
    SequencePushN,    // register, start register, count
    SequenceToList,   // register
    SequenceToTuple,  // register
    StringStart,      // register
    StringPush,       // register, value register
    StringFinish,     // register
    SimpleFunction,   // register, arg count, size[2]
    Function,         // register, arg count, capture count, flags, size[2]
    Capture,          // function, target, source
    Range,            // register, start, end
    RangeInclusive,   // register, start, end
    RangeTo,          // register, end
    RangeToInclusive, // register, end
    RangeFrom,        // register, start
    RangeFull,        // register
    Negate,           // register, source
    Add,              // result, lhs, rhs
    Subtract,         // result, lhs, rhs
    Multiply,         // result, lhs, rhs
    Divide,           // result, lhs, rhs
    Modulo,           // result, lhs, rhs
    Less,             // result, lhs, rhs
    LessOrEqual,      // result, lhs, rhs
    Greater,          // result, lhs, rhs
    GreaterOrEqual,   // result, lhs, rhs
    Equal,            // result, lhs, rhs
    NotEqual,         // result, lhs, rhs
    Jump,             // offset[2]
    JumpTrue,         // condition, offset[2]
    JumpFalse,        // condition, offset[2]
    JumpBack,         // offset[2]
    JumpBackFalse,    // offset[2]
    Call,             // result, function, arg register, arg count
    CallChild,        // result, function, arg register, arg count, parent
    Return,           // register
    Yield,            // register
    Throw,            // register
    IterNext,         // output, iterator, jump offset[2]
    IterNextTemp,     // output, iterator, jump offset[2]
    IterNextQuiet,    // iterator, jump offset[2]
    ValueIndex,       // result, value register, signed index
    SliceFrom,        // result, value register, signed index
    SliceTo,          // result, value register, signed index
    Index,            // result, indexable, index
    SetIndex,         // indexable, index, value
    MapInsert,        // map, key, value
    MetaInsert,       // map register, key id, value register
    MetaInsertNamed,  // map register, key id, name register, value register
    MetaExport,       // key id, value register
    MetaExportNamed,  // key id, name register, value register
    ValueExport,      // name, value
    Access,           // result, value, key
    IsList,           // register, value
    IsTuple,          // register, value
    Size,             // register, value
    TryStart,         // catch arg register, catch body offset[2]
    TryEnd,           //
    Debug,            // register, constant[3]
    CheckType,        // register, type (see TypeId)
    CheckSize,        // register, size
    Unused89,
    Unused90,
    Unused91,
    Unused92,
    Unused93,
    Unused94,
    Unused95,
    Unused96,
    Unused97,
    Unused98,
    Unused99,
    Unused100,
    Unused101,
    Unused102,
    Unused103,
    Unused104,
    Unused105,
    Unused106,
    Unused107,
    Unused108,
    Unused109,
    Unused110,
    Unused111,
    Unused112,
    Unused113,
    Unused114,
    Unused115,
    Unused116,
    Unused117,
    Unused118,
    Unused119,
    Unused120,
    Unused121,
    Unused122,
    Unused123,
    Unused124,
    Unused125,
    Unused126,
    Unused127,
    Unused128,
    Unused129,
    Unused130,
    Unused131,
    Unused132,
    Unused133,
    Unused134,
    Unused135,
    Unused136,
    Unused137,
    Unused138,
    Unused139,
    Unused140,
    Unused141,
    Unused142,
    Unused143,
    Unused144,
    Unused145,
    Unused146,
    Unused147,
    Unused148,
    Unused149,
    Unused150,
    Unused151,
    Unused152,
    Unused153,
    Unused154,
    Unused155,
    Unused156,
    Unused157,
    Unused158,
    Unused159,
    Unused160,
    Unused161,
    Unused162,
    Unused163,
    Unused164,
    Unused165,
    Unused166,
    Unused167,
    Unused168,
    Unused169,
    Unused170,
    Unused171,
    Unused172,
    Unused173,
    Unused174,
    Unused175,
    Unused176,
    Unused177,
    Unused178,
    Unused179,
    Unused180,
    Unused181,
    Unused182,
    Unused183,
    Unused184,
    Unused185,
    Unused186,
    Unused187,
    Unused188,
    Unused189,
    Unused190,
    Unused191,
    Unused192,
    Unused193,
    Unused194,
    Unused195,
    Unused196,
    Unused197,
    Unused198,
    Unused199,
    Unused200,
    Unused201,
    Unused202,
    Unused203,
    Unused204,
    Unused205,
    Unused206,
    Unused207,
    Unused208,
    Unused209,
    Unused210,
    Unused211,
    Unused212,
    Unused213,
    Unused214,
    Unused215,
    Unused216,
    Unused217,
    Unused218,
    Unused219,
    Unused220,
    Unused221,
    Unused222,
    Unused223,
    Unused224,
    Unused225,
    Unused226,
    Unused227,
    Unused228,
    Unused229,
    Unused230,
    Unused231,
    Unused232,
    Unused233,
    Unused234,
    Unused235,
    Unused236,
    Unused237,
    Unused238,
    Unused239,
    Unused240,
    Unused241,
    Unused242,
    Unused243,
    Unused244,
    Unused245,
    Unused246,
    Unused247,
    Unused248,
    Unused249,
    Unused250,
    Unused251,
    Unused252,
    Unused253,
    Unused254,
    Unused255,
}

impl From<u8> for Op {
    fn from(op: u8) -> Op {
        // Safety:
        //  - Op is repr(u8)
        //  - All 256 possible values are represented in the enum
        unsafe { std::mem::transmute(op) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_op_count() {
        assert_eq!(
            Op::Unused255 as u8,
            255,
            "Op should have 256 entries (see impl From<u8> for Op)"
        );
    }
}
