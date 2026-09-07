#!/usr/bin/env python3
"""Generates typed Python and Dart model definitions from the TypeScript ones.

Reads the ts-rs output at bindings/obby-wasm/src/types.d.ts, the only generated source of truth
for the wire shapes, and emits the same shapes into bindings/obby-python/obby_client.pyi and
bindings/obby-dart/lib/src/model.dart. Run through `make types`, never by hand.

The parser only understands the subset of TypeScript ts-rs actually emits for this project: object
types, unions of object types discriminated by a "type" or "mechanism" field, unions of string
literals, Array<T>, index-signature records, T | null, optional fields, and the primitives string,
number, boolean and bigint. Anything outside that raises GeneratorError rather than guessing, since
a silent misparse would ship a wrong type to every host.
"""

import keyword
import re
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TYPES_DTS = REPO_ROOT / "bindings/obby-wasm/src/types.d.ts"
PYI_PATH = REPO_ROOT / "bindings/obby-python/obby_client.pyi"
DART_MODEL_PATH = REPO_ROOT / "bindings/obby-dart/lib/src/model.dart"

PRIMITIVES = {"string", "number", "boolean", "bigint"}


class GeneratorError(Exception):
    pass


# --- tokenizer -----------------------------------------------------------------------------


@dataclass
class Token:
    kind: str  # 'doc' | 'ident' | 'string' | 'punct' | 'eof'
    value: str


PUNCT = set("{}[]<>|:;,?=")


def tokenize(text: str) -> list[Token]:
    tokens: list[Token] = []
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch.isspace():
            i += 1
            continue
        if text.startswith("/**", i):
            end = text.index("*/", i + 3)
            raw = text[i + 3 : end]
            lines = [line.strip().lstrip("*").strip() for line in raw.splitlines()]
            doc = "\n".join(line for line in lines if line)
            tokens.append(Token("doc", doc))
            i = end + 2
            continue
        if text.startswith("//", i):
            i = text.index("\n", i) if "\n" in text[i:] else n
            continue
        if ch == '"':
            end = text.index('"', i + 1)
            tokens.append(Token("string", text[i + 1 : end]))
            i = end + 1
            continue
        if ch.isalpha() or ch == "_":
            j = i
            while j < n and (text[j].isalnum() or text[j] == "_"):
                j += 1
            tokens.append(Token("ident", text[i:j]))
            i = j
            continue
        if ch in PUNCT:
            tokens.append(Token("punct", ch))
            i += 1
            continue
        raise GeneratorError(f"unexpected character {ch!r} at offset {i}")
    tokens.append(Token("eof", ""))
    return tokens


# --- AST -------------------------------------------------------------------------------------


@dataclass
class PrimitiveType:
    name: str


@dataclass
class ReferenceType:
    name: str


@dataclass
class ArrayType:
    inner: object


@dataclass
class OptionalType:
    inner: object


@dataclass
class RecordType:
    key_type: str
    value: object


@dataclass
class LiteralType:
    value: str


@dataclass
class NamedField:
    name: str
    optional: bool
    type: object
    doc: str | None


@dataclass
class RecordField:
    key_type: str
    value: object


@dataclass
class ObjectDecl:
    name: str
    doc: str | None
    fields: list[NamedField]


@dataclass
class AliasDecl:
    name: str
    doc: str | None
    target: object


@dataclass
class StringUnionDecl:
    name: str
    doc: str | None
    variants: list[str]


@dataclass
class UnionVariant:
    tag: str
    doc: str | None
    fields: list[NamedField]


@dataclass
class UnionDecl:
    name: str
    doc: str | None
    discriminant: str
    variants: list[UnionVariant]


# raw (unresolved) union-alternative nodes, used only while parsing one '|'-separated list
@dataclass
class _NullNode:
    pass


@dataclass
class _StringLiteralNode:
    value: str


@dataclass
class _ReferenceNode:
    name: str


@dataclass
class _ArrayNode:
    inner: object


@dataclass
class _ObjectNode:
    members: list
    doc: str | None


# --- parser ------------------------------------------------------------------------------------


class Parser:
    def __init__(self, tokens: list[Token]):
        self.tokens = tokens
        self.pos = 0

    def peek(self) -> Token:
        return self.tokens[self.pos]

    def next(self) -> Token:
        tok = self.tokens[self.pos]
        self.pos += 1
        return tok

    def take_doc(self) -> str | None:
        if self.peek().kind == "doc":
            return self.next().value
        return None

    def expect_punct(self, value: str) -> None:
        tok = self.next()
        if tok.kind != "punct" or tok.value != value:
            raise GeneratorError(f"expected {value!r}, got {tok.kind} {tok.value!r}")

    def expect_ident(self, value: str) -> None:
        tok = self.next()
        if tok.kind != "ident" or tok.value != value:
            raise GeneratorError(f"expected ident {value!r}, got {tok.kind} {tok.value!r}")

    def expect_ident_any(self) -> str:
        tok = self.next()
        if tok.kind != "ident":
            raise GeneratorError(f"expected an identifier, got {tok.kind} {tok.value!r}")
        return tok.value

    def peek_punct(self, value: str) -> bool:
        tok = self.peek()
        return tok.kind == "punct" and tok.value == value

    def peek_ident(self, value: str) -> bool:
        tok = self.peek()
        return tok.kind == "ident" and tok.value == value

    # -- grammar --

    def parse_file(self) -> list:
        decls = []
        while self.peek().kind != "eof":
            decls.append(self.parse_decl())
        return decls

    def parse_decl(self):
        doc = self.take_doc()
        self.expect_ident("export")
        self.expect_ident("type")
        name = self.expect_ident_any()
        self.expect_punct("=")
        alts = self.parse_alt_list()
        self.expect_punct(";")
        return self._build_decl(name, doc, alts)

    def parse_alt_list(self) -> list:
        nodes = [self.parse_primary_node()]
        while self.peek_punct("|"):
            self.next()
            nodes.append(self.parse_primary_node())
        return nodes

    def parse_primary_node(self):
        if self.peek_punct("{"):
            return self.parse_object_node()
        if self.peek().kind == "string":
            return _StringLiteralNode(self.next().value)
        if self.peek_ident("null"):
            self.next()
            return _NullNode()
        if self.peek_ident("Array"):
            self.next()
            self.expect_punct("<")
            inner = self.parse_type()
            self.expect_punct(">")
            return _ArrayNode(inner)
        if self.peek().kind == "ident":
            return _ReferenceNode(self.next().value)
        tok = self.peek()
        raise GeneratorError(f"unexpected token {tok.kind} {tok.value!r} in type position")

    def parse_object_node(self) -> _ObjectNode:
        self.expect_punct("{")
        members = []
        while not self.peek_punct("}"):
            doc = self.take_doc()
            if self.peek_punct("["):
                self.next()
                self.expect_ident_any()  # the placeholder name, e.g. `key`
                self.expect_ident("in")
                key_type = self.expect_ident_any()
                self.expect_punct("]")
                if self.peek_punct("?"):
                    self.next()
                self.expect_punct(":")
                value_type = self.parse_type()
                if self.peek_punct(","):
                    self.next()
                members.append(RecordField(key_type, value_type))
                continue
            if self.peek().kind == "string":
                field_name = self.next().value
            else:
                field_name = self.expect_ident_any()
            optional = False
            if self.peek_punct("?"):
                self.next()
                optional = True
            self.expect_punct(":")
            field_type = self.parse_type()
            if self.peek_punct(","):
                self.next()
            members.append(NamedField(field_name, optional, field_type, doc))
        self.expect_punct("}")
        return _ObjectNode(members, None)

    def parse_type(self):
        alts = self.parse_alt_list()
        return self._resolve_alts(alts)

    # -- resolution --

    def _resolve_alts(self, alts: list):
        if len(alts) == 1:
            return self._resolve_primary(alts[0])
        nulls = [a for a in alts if isinstance(a, _NullNode)]
        others = [a for a in alts if not isinstance(a, _NullNode)]
        if len(alts) == 2 and len(nulls) == 1:
            return OptionalType(self._resolve_primary(others[0]))
        raise GeneratorError(f"unsupported type union with {len(alts)} alternatives: {alts}")

    def _resolve_primary(self, node):
        if isinstance(node, _StringLiteralNode):
            return LiteralType(node.value)
        if isinstance(node, _ReferenceNode):
            if node.name in PRIMITIVES:
                return PrimitiveType(node.name)
            return ReferenceType(node.name)
        if isinstance(node, _ArrayNode):
            return ArrayType(node.inner)
        if isinstance(node, _ObjectNode):
            if len(node.members) == 1 and isinstance(node.members[0], RecordField):
                member = node.members[0]
                return RecordType(member.key_type, member.value)
            raise GeneratorError(
                "an inline object type is only supported as a top-level declaration or as a "
                "discriminated-union variant, not nested inside a field type"
            )
        raise GeneratorError(f"cannot resolve {node!r} to a field type")

    def _build_decl(self, name: str, doc: str | None, alts: list):
        if len(alts) == 1:
            node = alts[0]
            if isinstance(node, _ObjectNode):
                if len(node.members) == 1 and isinstance(node.members[0], RecordField):
                    member = node.members[0]
                    return AliasDecl(name, doc, RecordType(member.key_type, member.value))
                if not all(isinstance(m, NamedField) for m in node.members):
                    raise GeneratorError(f"{name} mixes record and named fields")
                return ObjectDecl(name, doc, list(node.members))
            return AliasDecl(name, doc, self._resolve_primary(node))

        if all(isinstance(a, _StringLiteralNode) for a in alts):
            return StringUnionDecl(name, doc, [a.value for a in alts])

        if all(isinstance(a, _ObjectNode) for a in alts):
            return self._build_union(name, doc, alts)

        raise GeneratorError(f"{name} is a union that is neither all string literals nor all objects")

    def _build_union(self, name: str, doc: str | None, alts: list[_ObjectNode]) -> UnionDecl:
        discriminant = None
        variants = []
        for node in alts:
            if not node.members or not isinstance(node.members[0], NamedField):
                raise GeneratorError(f"{name} has a variant with no discriminant field")
            head = node.members[0]
            if head.name not in ("type", "mechanism"):
                raise GeneratorError(
                    f"{name} variant discriminant must be named 'type' or 'mechanism', got "
                    f"{head.name!r}"
                )
            if discriminant is None:
                discriminant = head.name
            elif discriminant != head.name:
                raise GeneratorError(f"{name} mixes discriminant fields {discriminant!r}/{head.name!r}")
            if not isinstance(head.type, LiteralType):
                raise GeneratorError(f"{name} discriminant field must be a string literal")
            rest = node.members[1:]
            if not all(isinstance(m, NamedField) for m in rest):
                raise GeneratorError(f"{name} variant {head.type.value!r} mixes in a record field")
            variants.append(UnionVariant(head.type.value, node.doc, list(rest)))
        assert discriminant is not None
        return UnionDecl(name, doc, discriminant, variants)


def parse_types(text: str) -> list:
    return Parser(tokenize(text)).parse_file()


# --- naming --------------------------------------------------------------------------------


def pascal(snake: str) -> str:
    return "".join(part.capitalize() for part in snake.split("_"))


def variant_class_name(union_name: str, tag: str) -> str:
    return f"{union_name}{pascal(tag)}"


# --- Python codegen --------------------------------------------------------------------------


def py_type(ref) -> str:
    if isinstance(ref, PrimitiveType):
        return {"string": "str", "number": "int", "boolean": "bool", "bigint": "int"}[ref.name]
    if isinstance(ref, ReferenceType):
        return ref.name
    if isinstance(ref, ArrayType):
        return f"list[{py_type(ref.inner)}]"
    if isinstance(ref, OptionalType):
        return f"{py_type(ref.inner)} | None"
    if isinstance(ref, RecordType):
        key = "str" if ref.key_type == "string" else ref.key_type
        return f"dict[{key}, {py_type(ref.value)}]"
    if isinstance(ref, LiteralType):
        return f'Literal["{ref.value}"]'
    raise GeneratorError(f"no Python type for {ref!r}")


def py_field_lines(fields: list[NamedField], indent: str = "    ") -> list[str]:
    lines = []
    for field in fields:
        lines.append(f"{indent}{field.name}: {py_type(field.type)}")
    return lines


def _has_keyword_field(fields: list[NamedField]) -> bool:
    return any(keyword.iskeyword(f.name) for f in fields)


def _functional_typeddict(name: str, fields: list[NamedField]) -> str:
    # a field like ModelChange's `from` is a Python keyword, so it cannot be a class-body
    # attribute name; TypedDict's functional form takes it as a plain string key instead
    items = ", ".join(f'"{f.name}": {py_type(f.type)}' for f in fields)
    return f'{name} = TypedDict("{name}", {{{items}}})'


def py_object_decl(decl: ObjectDecl) -> list[str]:
    required = [f for f in decl.fields if not f.optional]
    optional = [f for f in decl.fields if f.optional]
    if not optional:
        if _has_keyword_field(decl.fields):
            return [_functional_typeddict(decl.name, decl.fields)]
        lines = [f"class {decl.name}(TypedDict):"]
        lines += py_field_lines(decl.fields) or ["    pass"]
        return lines
    if _has_keyword_field(decl.fields):
        raise GeneratorError(
            f"{decl.name} mixes optional fields with a Python-keyword field name, which the "
            "functional TypedDict form cannot express as a total=False subclass"
        )
    lines = [f"class _{decl.name}Required(TypedDict):"]
    lines += py_field_lines(required) or ["    pass"]
    lines.append("")
    lines.append(f"class {decl.name}(_{decl.name}Required, total=False):")
    lines += py_field_lines(optional)
    return lines


def py_string_union_decl(decl: StringUnionDecl) -> list[str]:
    literal = ", ".join(f'"{v}"' for v in decl.variants)
    return [f"{decl.name} = Literal[{literal}]"]


def py_union_decl(decl: UnionDecl) -> list[str]:
    lines = []
    variant_names = []
    for variant in decl.variants:
        cls = variant_class_name(decl.name, variant.tag)
        variant_names.append(cls)
        discriminant_field = NamedField(decl.discriminant, False, LiteralType(variant.tag), None)
        all_fields = [discriminant_field, *variant.fields]
        if _has_keyword_field(variant.fields):
            lines.append(_functional_typeddict(cls, all_fields))
        else:
            lines.append(f"class {cls}(TypedDict):")
            lines += py_field_lines(all_fields)
        lines.append("")
    lines.append(f"{decl.name} = Union[{', '.join(variant_names)}]")
    return lines


def py_alias_decl(decl: AliasDecl) -> list[str]:
    return [f"{decl.name} = {py_type(decl.target)}"]


PYI_SECTION_START = "# --- generated model types: start ---"
PYI_SECTION_END = "# --- generated model types: end ---"


def render_pyi_section(decls: list) -> str:
    lines = [
        PYI_SECTION_START,
        "#",
        "# Generated by scripts/generate-model-types.py from bindings/obby-wasm/src/types.d.ts.",
        "# Do not edit by hand.",
        "",
        "from typing import Literal, TypedDict, Union",
        "",
    ]
    for decl in decls:
        if isinstance(decl, ObjectDecl):
            lines += py_object_decl(decl)
        elif isinstance(decl, StringUnionDecl):
            lines += py_string_union_decl(decl)
        elif isinstance(decl, UnionDecl):
            lines += py_union_decl(decl)
        elif isinstance(decl, AliasDecl):
            lines += py_alias_decl(decl)
        else:
            raise GeneratorError(f"unhandled decl {decl!r}")
        lines.append("")
        lines.append("")
    while lines and lines[-1] == "":
        lines.pop()
    lines.append("")
    lines.append(PYI_SECTION_END)
    return "\n".join(lines)


def update_pyi(decls: list) -> None:
    original = PYI_PATH.read_text()
    section = render_pyi_section(decls)
    if PYI_SECTION_START in original:
        pattern = re.compile(
            re.escape(PYI_SECTION_START) + r".*?" + re.escape(PYI_SECTION_END), re.DOTALL
        )
        updated = pattern.sub(section, original)
    else:
        updated = original.rstrip("\n") + "\n\n" + section + "\n"
    PYI_PATH.write_text(updated)


# --- Dart codegen ----------------------------------------------------------------------------


DART_OBJECT_NAMES: set[str] = set()  # populated once decls are known, for reference resolution
DART_UNION_NAMES: set[str] = set()
DART_ENUM_NAMES: set[str] = set()
# a name alias for Array<T> (only MessageTags), where the Dart typedef is transparent for type
# annotations but decoding still has to rebuild the list: a JSON array cannot be reinterpreted as
# a typed List with a cast the way a primitive alias like CaseFolded can.
DART_ARRAY_ALIAS_ELEMENT: dict[str, object] = {}


def dart_type(ref) -> str:
    if isinstance(ref, PrimitiveType):
        return {"string": "String", "number": "int", "boolean": "bool", "bigint": "int"}[ref.name]
    if isinstance(ref, ReferenceType):
        return ref.name
    if isinstance(ref, ArrayType):
        return f"List<{dart_type(ref.inner)}>"
    if isinstance(ref, OptionalType):
        return f"{dart_type(ref.inner)}?"
    if isinstance(ref, RecordType):
        key = "String" if ref.key_type == "string" else dart_type(ReferenceType(ref.key_type))
        return f"Map<{key}, {dart_type(ref.value)}>"
    raise GeneratorError(f"no Dart type for {ref!r}")


def dart_decode_expr(ref, json_expr: str) -> str:
    """A Dart expression that reads `ref`'s value out of a `dynamic` json_expr."""
    if isinstance(ref, OptionalType):
        inner_expr = dart_decode_expr(ref.inner, json_expr)
        return f"{json_expr} == null ? null : {inner_expr}"
    if isinstance(ref, PrimitiveType):
        return f"{json_expr} as {dart_type(ref)}"
    if isinstance(ref, ReferenceType):
        name = ref.name
        if name in DART_ARRAY_ALIAS_ELEMENT:
            return dart_decode_expr(ArrayType(DART_ARRAY_ALIAS_ELEMENT[name]), json_expr)
        if name in DART_ENUM_NAMES:
            return f"{name}.fromWire({json_expr} as String)"
        if name in DART_OBJECT_NAMES or name in DART_UNION_NAMES:
            return f"{name}.fromJson({json_expr} as Map<String, dynamic>)"
        # a plain alias to a primitive, e.g. CaseFolded; the typedef is transparent, so a cast
        # to it and a cast to what it aliases are the same operation
        return f"{json_expr} as {dart_type(ref)}"
    if isinstance(ref, ArrayType):
        inner_expr = dart_decode_expr(ref.inner, "e")
        return f"({json_expr} as List).map((e) => {inner_expr}).toList()"
    if isinstance(ref, RecordType):
        value_expr = dart_decode_expr(ref.value, "v")
        return f"({json_expr} as Map<String, dynamic>).map((k, v) => MapEntry(k, {value_expr}))"
    raise GeneratorError(f"no Dart decode expression for {ref!r}")


def dart_encode_expr(ref, value_expr: str, nullable: bool = False) -> str:
    """A Dart expression that turns a value typed `ref` back into something `jsonEncode` accepts
    directly, the inverse of `dart_decode_expr`. Needed wherever a generated type is also built by
    a host to send outbound, such as `VoiceSignal` for `sendVoiceSignal`.

    `nullable` picks `?.` over `.` for the access directly on `value_expr`: a null check can only
    promote a local variable or a private field, never a public one (an override could return a
    different value on a second read), so a plain `== null ? null : value_expr.foo()` does not
    type-check on the public fields these classes declare. Dart's null-shorting means only that
    first access needs the `?`; every further `.` chained onto the same expression already short
    -circuits with it.
    """
    op = "?." if nullable else "."
    if isinstance(ref, OptionalType):
        return dart_encode_expr(ref.inner, value_expr, nullable=True)
    if isinstance(ref, PrimitiveType):
        return value_expr
    if isinstance(ref, ReferenceType):
        name = ref.name
        if name in DART_ARRAY_ALIAS_ELEMENT:
            return dart_encode_expr(ArrayType(DART_ARRAY_ALIAS_ELEMENT[name]), value_expr, nullable)
        if name in DART_ENUM_NAMES:
            return f"{value_expr}{op}wire"
        if name in DART_OBJECT_NAMES or name in DART_UNION_NAMES:
            return f"{value_expr}{op}toJson()"
        return value_expr
    if isinstance(ref, ArrayType):
        inner_expr = dart_encode_expr(ref.inner, "e")
        return f"{value_expr}{op}map((e) => {inner_expr}).toList()"
    if isinstance(ref, RecordType):
        value_expr2 = dart_encode_expr(ref.value, "v")
        return f"{value_expr}{op}map((k, v) => MapEntry(k, {value_expr2}))"
    raise GeneratorError(f"no Dart encode expression for {ref!r}")


def _effective(field: NamedField):
    """A field marked optional may be absent from the JSON entirely, which reads back as Dart
    `null` from a plain map index, so its type and decode both have to be nullable regardless of
    what the TypeScript field type itself says."""
    return OptionalType(field.type) if field.optional else field.type


def dart_doc(doc: str | None, indent: str = "") -> list[str]:
    if not doc:
        return []
    return [f"{indent}/// {line}" for line in doc.splitlines()]


def dart_object_decl(decl: ObjectDecl) -> list[str]:
    lines = dart_doc(decl.doc)
    lines.append(f"class {decl.name} {{")
    lines.append(f"  const {decl.name}({{")
    for field in decl.fields:
        required = "" if field.optional else "required "
        lines.append(f"    {required}this.{field.name},")
    lines.append("  });")
    lines.append("")
    lines.append(f"  factory {decl.name}.fromJson(Map<String, dynamic> json) => {decl.name}(")
    for field in decl.fields:
        json_expr = f"json['{field.name}']"
        decode = dart_decode_expr(_effective(field), json_expr)
        lines.append(f"    {field.name}: {decode},")
    lines.append("  );")
    lines.append("")
    lines.append("  Map<String, dynamic> toJson() => {")
    for field in decl.fields:
        encode = dart_encode_expr(_effective(field), field.name)
        lines.append(f"    '{field.name}': {encode},")
    lines.append("  };")
    lines.append("")
    for field in decl.fields:
        lines += dart_doc(field.doc, "  ")
        lines.append(f"  final {dart_type(_effective(field))} {field.name};")
        lines.append("")
    while lines[-1] == "":
        lines.pop()
    lines.append("}")
    return lines


def dart_string_union_decl(decl: StringUnionDecl) -> list[str]:
    lines = dart_doc(decl.doc)
    lines.append(f"enum {decl.name} {{")
    # an enhanced enum needs a `;` closing the value list before any member, not just a trailing
    # comma, so the last value's terminator differs from the rest
    for i, variant in enumerate(decl.variants):
        terminator = ";" if i == len(decl.variants) - 1 else ","
        lines.append(f"  {variant}{terminator}")
    lines.append("")
    lines.append(f"  static {decl.name} fromWire(String wire) => {decl.name}.values.byName(wire);")
    lines.append("")
    lines.append("  String get wire => name;")
    lines.append("}")
    return lines


def dart_union_decl(decl: UnionDecl) -> list[str]:
    lines = dart_doc(decl.doc)
    lines.append(f"sealed class {decl.name} {{")
    lines.append(f"  const {decl.name}();")
    lines.append("")
    lines.append("  Map<String, dynamic> toJson();")
    lines.append("")
    lines.append(f"  factory {decl.name}.fromJson(Map<String, dynamic> json) {{")
    lines.append(f"    final tag = json['{decl.discriminant}'] as String;")
    lines.append("    return switch (tag) {")
    for variant in decl.variants:
        cls = variant_class_name(decl.name, variant.tag)
        lines.append(f"      '{variant.tag}' => {cls}.fromJson(json),")
    lines.append(
        f"      _ => throw ArgumentError.value(tag, '{decl.discriminant}', "
        f"'unknown {decl.name} variant'),"
    )
    lines.append("    };")
    lines.append("  }")
    lines.append("}")
    lines.append("")

    for variant in decl.variants:
        cls = variant_class_name(decl.name, variant.tag)
        lines += dart_doc(variant.doc)
        lines.append(f"class {cls} extends {decl.name} {{")
        if not variant.fields:
            lines.append(f"  const {cls}();")
            lines.append("")
            lines.append(f"  factory {cls}.fromJson(Map<String, dynamic> json) => const {cls}();")
            lines.append("")
            lines.append("  @override")
            lines.append(f"  Map<String, dynamic> toJson() => {{'{decl.discriminant}': '{variant.tag}'}};")
            lines.append("}")
            lines.append("")
            continue
        lines.append(f"  const {cls}({{")
        for field in variant.fields:
            required = "" if field.optional else "required "
            lines.append(f"    {required}this.{field.name},")
        lines.append("  });")
        lines.append("")
        lines.append(f"  factory {cls}.fromJson(Map<String, dynamic> json) => {cls}(")
        for field in variant.fields:
            json_expr = f"json['{field.name}']"
            decode = dart_decode_expr(_effective(field), json_expr)
            lines.append(f"    {field.name}: {decode},")
        lines.append("  );")
        lines.append("")
        lines.append("  @override")
        lines.append("  Map<String, dynamic> toJson() => {")
        lines.append(f"    '{decl.discriminant}': '{variant.tag}',")
        for field in variant.fields:
            encode = dart_encode_expr(_effective(field), field.name)
            lines.append(f"    '{field.name}': {encode},")
        lines.append("  };")
        lines.append("")
        for field in variant.fields:
            lines += dart_doc(field.doc, "  ")
            lines.append(f"  final {dart_type(_effective(field))} {field.name};")
            lines.append("")
        while lines[-1] == "":
            lines.pop()
        lines.append("}")
        lines.append("")
    while lines[-1] == "":
        lines.pop()
    return lines


def dart_alias_decl(decl: AliasDecl) -> list[str] | None:
    if isinstance(decl.target, (PrimitiveType, ReferenceType)):
        lines = dart_doc(decl.doc)
        lines.append(f"typedef {decl.name} = {dart_type(decl.target)};")
        return lines
    if isinstance(decl.target, ArrayType):
        lines = dart_doc(decl.doc)
        lines.append(f"typedef {decl.name} = List<{dart_type(decl.target.inner)}>;")
        return lines
    raise GeneratorError(f"no Dart alias form for {decl!r}")


def render_dart_model(decls: list) -> str:
    for decl in decls:
        if isinstance(decl, ObjectDecl):
            DART_OBJECT_NAMES.add(decl.name)
        elif isinstance(decl, UnionDecl):
            DART_UNION_NAMES.add(decl.name)
        elif isinstance(decl, StringUnionDecl):
            DART_ENUM_NAMES.add(decl.name)
        elif isinstance(decl, AliasDecl) and isinstance(decl.target, ArrayType):
            DART_ARRAY_ALIAS_ELEMENT[decl.name] = decl.target.inner

    lines = [
        "// Generated by scripts/generate-model-types.py from bindings/obby-wasm/src/types.d.ts.",
        "// Do not edit by hand.",
        "//",
        "// Field names mirror the wire's snake_case rather than being recased to lowerCamelCase, so",
        "// this file keeps its own naming and null-check style rather than the package's.",
        "// ignore_for_file: non_constant_identifier_names, constant_identifier_names",
        "// ignore_for_file: prefer_if_null_operators, prefer_null_aware_operators",
        "library;",
        "",
    ]
    first = True
    for decl in decls:
        if not first:
            lines.append("")
        first = False
        if isinstance(decl, ObjectDecl):
            lines += dart_object_decl(decl)
        elif isinstance(decl, StringUnionDecl):
            lines += dart_string_union_decl(decl)
        elif isinstance(decl, UnionDecl):
            lines += dart_union_decl(decl)
        elif isinstance(decl, AliasDecl):
            rendered = dart_alias_decl(decl)
            if rendered is None:
                first = True  # nothing emitted, don't leave a stray blank line
                continue
            lines += rendered
        else:
            raise GeneratorError(f"unhandled decl {decl!r}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    text = TYPES_DTS.read_text()
    try:
        decls = parse_types(text)
    except GeneratorError as err:
        print(f"generate-model-types: {err}", file=sys.stderr)
        return 1

    update_pyi(decls)
    DART_MODEL_PATH.write_text(render_dart_model(decls))
    print(f"generated {len(decls)} types into {PYI_PATH} and {DART_MODEL_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
