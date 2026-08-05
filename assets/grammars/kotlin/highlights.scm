; Kotlin highlight query written for Apex against tree-sitter-kotlin-ng 1.1.
; Licensed with Apex (AGPL-3.0-or-later); the grammar itself is MIT.

(line_comment) @comment
(block_comment) @comment
(shebang) @comment

(string_literal) @string
(multiline_string_literal) @string
(character_literal) @string
(escape_sequence) @string.escape
(interpolation) @punctuation.special

(number_literal) @number
(float_literal) @number

(annotation) @attribute
(file_annotation) @attribute
(label) @label

(user_type (identifier) @type)
(class_declaration (identifier) @type)
(object_declaration (identifier) @type)
(enum_entry (identifier) @constant)
(type_alias (identifier) @type)
(type_parameter (identifier) @type)

(function_declaration (identifier) @function)
(call_expression (identifier) @function)
(call_expression (navigation_expression (identifier) @function.method . ))
(navigation_expression (identifier) @property)
(parameter (identifier) @variable.parameter)
(class_parameter (identifier) @variable.parameter)
(lambda_parameters (variable_declaration (identifier) @variable.parameter))
(property_declaration (variable_declaration (identifier) @variable))
(package_header (qualified_identifier) @namespace)
(identifier) @variable

(this_expression) @variable.builtin
(super_expression) @variable.builtin

["fun" "val" "var" "class" "object" "interface" "typealias" "constructor" "init" "companion" "enum" "sealed" "data" "annotation" "abstract" "open" "final" "override" "lateinit" "const" "inline" "infix" "operator" "suspend" "tailrec" "inner" "internal" "private" "protected" "public" "external" "expect" "actual" "crossinline" "noinline" "vararg" "by" "get" "set" "where" "out" "in" "is" "as" "as?" "!in" "!is"] @keyword

["if" "else" "when" "for" "while" "do" "try" "catch" "finally" "throw" "return" "return@" "import" "package"] @keyword.control

["+" "-" "*" "/" "%" "=" "==" "!=" "===" "!==" "<" ">" "<=" ">=" "&&" "||" "!" "!!" "?:" "?." ".." "..<" "->" "::" "++" "--" "+=" "-=" "*=" "/=" "%=" "?"] @operator

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," ";" "." ":"] @punctuation.delimiter
