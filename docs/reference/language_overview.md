# Language Overview

Koto is an expression-based scripting language with the goals of keeping things
simple, and visually clean. Effort has been taken to only introduce visual
elements to the syntax when absolutely necessary.

## Assignments

Assignment to an identifier is performed using the `=` operator.

```koto
x = 1
a, b, c = 1, 2, 3
```

## Identifiers

Identifier names use the Unicode `xid_start` and `xid_continue` definitions as
outlined [here](https://unicode.org/reports/tr31/).

Examples of valid identifiers include:

```koto
my_id = 123
foo42 = 99
héllø = -1
やあ = 100
```

## Comments

Single-line comments are using a `#` symbol.

Multi-line comments are started with `#-` and finished with `-#`.

Mulii-line comments can be nested, and can be used as inline comments.

```koto
# This is a single-line comment

#- This is a
multi-line comment

#- Multi-line comments can be nested -#

-#

a = 1 + #- This is an inline comment -# 2
```

## Indentation

Whitespace is used to define indented blocks of code, and for continuing long
expressions.

Spaces and tabs are both counted as whitespace characters.

Any amount of indentation can be used for a block, but the level must remain
consistent throughout a block for the parser to understand that the expressions
belong together.

```koto
x = if foo                # An if expression, starting on column 1
  result = do_something() # An indented 'then' block for the if expression
  result                  # A continuation of the 'then' block
else                      # The else condition matching the 'if' indentation
  do_something_else()     # The 'else' body.

# No 'end if' is necessary here to start a new expression,
# the return of indentation to column 1
print x
```


## Text format

Koto scripts are expected to contain valid UTF-8 data.

