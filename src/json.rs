//! Modelo de documento JSON y parser tolerante (JSONC).
//!
//! - Acepta comentarios `//` y `/* */`, comas finales y BOM.
//! - Conserva el orden de las claves, claves duplicadas y el texto original
//!   de los números (p. ej. `15.000000` se muestra tal cual).
//! - Los nodos se guardan en un arreglo en preorden, por lo que los
//!   descendientes de un nodo forman un rango contiguo de ids.

use std::borrow::Cow;
use std::collections::HashMap;

pub type NodeId = u32;
pub const NONE: NodeId = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Object,
    Array,
    Str,
    Num,
    Bool,
    Null,
}

impl Kind {
    pub fn is_container(self) -> bool {
        matches!(self, Kind::Object | Kind::Array)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Object => "object",
            Kind::Array => "array",
            Kind::Str => "string",
            Kind::Num => "number",
            Kind::Bool => "bool",
            Kind::Null => "null",
        }
    }
}

/// Índice de una clave internada en [`Doc`]; claves iguales comparten índice.
pub type KeyId = u32;
pub const NO_KEY: KeyId = u32::MAX;

pub struct Node {
    pub kind: Kind,
    pub parent: NodeId,
    /// Posición dentro del padre.
    pub index: u32,
    pub depth: u32,
    /// Clave cuando el padre es un objeto (`NO_KEY` si no tiene).
    pub key: KeyId,
    /// Rango del valor escalar en `Doc::strs`: texto sin escapes para cadenas,
    /// texto original para números.
    text: (u32, u32),
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn has_key(&self) -> bool {
        self.key != NO_KEY
    }
}

/// Documento en memoria. Para que pese poco, las claves se internan y los
/// textos escalares viven en un único búfer en lugar de una asignación por nodo.
pub struct Doc {
    pub nodes: Vec<Node>,
    keys: Vec<Box<str>>,
    strs: String,
}

impl Doc {
    pub const ROOT: NodeId = 0;

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn key_of(&self, n: &Node) -> Option<&str> {
        (n.key != NO_KEY).then(|| &*self.keys[n.key as usize])
    }

    pub fn key_of_id(&self, k: KeyId) -> &str {
        &self.keys[k as usize]
    }

    pub fn text_of(&self, n: &Node) -> &str {
        &self.strs[n.text.0 as usize..(n.text.0 + n.text.1) as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Fin (exclusivo) del rango de ids del subárbol de `id`.
    pub fn subtree_end(&self, mut id: NodeId) -> NodeId {
        while let Some(&last) = self.node(id).children.last() {
            id = last;
        }
        id + 1
    }

    pub fn is_ancestor(&self, anc: NodeId, mut id: NodeId) -> bool {
        while id != NONE {
            if id == anc {
                return true;
            }
            id = self.node(id).parent;
        }
        false
    }

    /// Ruta estilo JavaScript: `$.Datos.Partidas[3].Codigo`.
    pub fn path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != NONE {
            let n = self.node(cur);
            if n.parent == NONE {
                break;
            }
            parts.push(match self.key_of(n) {
                Some(k) if is_ident(k) => format!(".{k}"),
                Some(k) => {
                    let mut s = String::from("[");
                    write_escaped(k, &mut s);
                    s.push(']');
                    s
                }
                None => format!("[{}]", n.index),
            });
            cur = n.parent;
        }
        let mut out = String::from("$");
        for p in parts.iter().rev() {
            out.push_str(p);
        }
        out
    }

    /// Dos nodos son "similares" si están en la misma ruta ignorando índices de arreglo.
    pub fn same_shape_path(&self, mut a: NodeId, mut b: NodeId) -> bool {
        loop {
            if a == b {
                return true;
            }
            if a == NONE || b == NONE {
                return false;
            }
            let (na, nb) = (self.node(a), self.node(b));
            if na.depth != nb.depth || na.key != nb.key {
                return false;
            }
            a = na.parent;
            b = nb.parent;
        }
    }

    /// Copia del documento con solo los nodos cuya clave está en `keys` (con todo
    /// su contenido) y el camino desde la raíz hasta ellos. Las claves se comparan
    /// sin distinguir mayúsculas.
    pub fn filter_keys(&self, keys: &[&str]) -> KeyFilter {
        let mut wanted: Vec<Option<usize>> = vec![None; self.keys.len()];
        for (id, k) in self.keys.iter().enumerate() {
            wanted[id] = keys.iter().position(|q| k.eq_ignore_ascii_case(q));
        }
        let hit = |node: &Node| (node.key != NO_KEY).then(|| wanted[node.key as usize]).flatten();
        let n = self.nodes.len();
        let mut counts = vec![0; keys.len()];
        let mut keep = vec![false; n];
        if n > 0 {
            keep[0] = true;
        }
        let mut i = 0;
        while i < n {
            let node = &self.nodes[i];
            match hit(node) {
                Some(q) => {
                    counts[q] += 1;
                    let end = self.subtree_end(i as NodeId) as usize;
                    keep[i..end].fill(true);
                    let mut p = node.parent;
                    while p != NONE && !keep[p as usize] {
                        keep[p as usize] = true;
                        p = self.node(p).parent;
                    }
                    i = end;
                }
                None => i += 1,
            }
        }

        // Los nodos del camino son los conservados que no están dentro de una coincidencia.
        let mut path = Vec::new();
        let mut map = vec![NONE; n];
        let mut nodes: Vec<Node> = Vec::new();
        let mut inside_end = 0;
        for (i, node) in self.nodes.iter().enumerate() {
            if !keep[i] {
                continue;
            }
            let id = nodes.len() as NodeId;
            map[i] = id;
            if i >= inside_end && hit(node).is_some() {
                inside_end = self.subtree_end(i as NodeId) as usize;
            }
            path.push(i >= inside_end);
            let (parent, index) = if node.parent == NONE {
                (NONE, 0)
            } else {
                let p = map[node.parent as usize];
                let pn = &mut nodes[p as usize];
                pn.children.push(id);
                (p, (pn.children.len() - 1) as u32)
            };
            nodes.push(Node {
                kind: node.kind,
                parent,
                index,
                depth: node.depth,
                key: node.key,
                text: node.text,
                children: Vec::new(),
            });
        }
        let doc = Doc { nodes, keys: self.keys.clone(), strs: self.strs.clone() };
        KeyFilter { doc, counts, path }
    }
}

/// Resultado de [`Doc::filter_keys`].
pub struct KeyFilter {
    pub doc: Doc,
    /// Coincidencias por cada clave pedida (sin contar las anidadas en otra coincidencia).
    pub counts: Vec<usize>,
    /// Por nodo del documento filtrado: `true` si solo está como parte del camino.
    pub path: Vec<bool>,
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

// ---------------------------------------------------------------------------
// Parser

#[derive(Clone, Debug)]
pub struct ParseError {
    pub msg: String,
    pub line: usize,
    pub col: usize,
    /// Desplazamiento en caracteres desde el inicio del texto.
    pub char_offset: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (línea {}, columna {})", self.msg, self.line, self.col)
    }
}

/// Convierte bytes leídos de un archivo a texto (UTF-8 o UTF-16 con BOM).
pub fn decode_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn parse(src: &str) -> Result<Doc, ParseError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser {
        src,
        s: src.as_bytes(),
        pos: 0,
        nodes: Vec::new(),
        stack: Vec::new(),
        keys: Vec::new(),
        key_ids: HashMap::new(),
        strs: String::new(),
    };
    p.run()?;
    p.nodes.shrink_to_fit();
    p.strs.shrink_to_fit();
    Ok(Doc { nodes: p.nodes, keys: p.keys, strs: p.strs })
}

struct Parser<'a> {
    src: &'a str,
    s: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
    stack: Vec<NodeId>,
    keys: Vec<Box<str>>,
    key_ids: HashMap<Box<str>, KeyId>,
    strs: String,
}

type PResult<T> = Result<T, ParseError>;

impl<'a> Parser<'a> {
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        self.err_at(self.pos, msg)
    }

    fn err_at<T>(&self, pos: usize, msg: impl Into<String>) -> PResult<T> {
        let mut pos = pos.min(self.s.len());
        while !self.src.is_char_boundary(pos) {
            pos -= 1;
        }
        let before = &self.src[..pos];
        let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let col = before[line_start..].chars().count() + 1;
        Err(ParseError { msg: msg.into(), line, col, char_offset: before.chars().count() })
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) -> PResult<()> {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'/' => match self.s.get(self.pos + 1) {
                    Some(b'/') => {
                        while let Some(c) = self.peek() {
                            if c == b'\n' {
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => {
                        let start = self.pos;
                        match self.src[self.pos + 2..].find("*/") {
                            Some(i) => self.pos += 2 + i + 2,
                            None => return self.err_at(start, "Comentario /* sin cerrar"),
                        }
                    }
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    fn run(&mut self) -> PResult<()> {
        self.ws()?;
        if self.peek().is_none() {
            return self.err("El documento está vacío");
        }
        self.value(NO_KEY, NONE)?;
        while let Some(&top) = self.stack.last() {
            self.ws()?;
            let is_obj = self.nodes[top as usize].kind == Kind::Object;
            let close = if is_obj { b'}' } else { b']' };
            match self.peek() {
                None => return self.err(format!("Fin inesperado: falta '{}'", close as char)),
                Some(c) if c == close => {
                    self.pos += 1;
                    self.stack.pop();
                    continue;
                }
                _ => {}
            }
            if !self.nodes[top as usize].children.is_empty() {
                if self.peek() != Some(b',') {
                    return self.err(format!("Se esperaba ',' o '{}'", close as char));
                }
                self.pos += 1;
                self.ws()?;
                if self.peek() == Some(close) {
                    // coma final
                    self.pos += 1;
                    self.stack.pop();
                    continue;
                }
            }
            let key = if is_obj {
                if self.peek() != Some(b'"') {
                    return self.err("Se esperaba una clave entre comillas");
                }
                let k = self.string()?;
                self.ws()?;
                if self.peek() != Some(b':') {
                    return self.err("Se esperaba ':'");
                }
                self.pos += 1;
                self.ws()?;
                self.intern(k)
            } else {
                NO_KEY
            };
            self.value(key, top)?;
        }
        self.ws()?;
        if self.pos < self.s.len() {
            return self.err("Contenido extra después del final del JSON");
        }
        Ok(())
    }

    fn intern(&mut self, k: Cow<'_, str>) -> KeyId {
        if let Some(&id) = self.key_ids.get(k.as_ref()) {
            return id;
        }
        let id = self.keys.len() as KeyId;
        let k: Box<str> = k.into();
        self.keys.push(k.clone());
        self.key_ids.insert(k, id);
        id
    }

    /// Guarda un texto escalar en el búfer compartido.
    fn store(&mut self, s: &str) -> (u32, u32) {
        let start = self.strs.len() as u32;
        self.strs.push_str(s);
        (start, s.len() as u32)
    }

    fn value(&mut self, key: KeyId, parent: NodeId) -> PResult<()> {
        let (kind, text) = match self.peek() {
            None => return self.err("Se esperaba un valor"),
            Some(b'{') => {
                self.pos += 1;
                (Kind::Object, (0, 0))
            }
            Some(b'[') => {
                self.pos += 1;
                (Kind::Array, (0, 0))
            }
            Some(b'"') => {
                let s = self.string()?;
                (Kind::Str, self.store(&s))
            }
            Some(b't') => (Kind::Bool, self.literal("true")?),
            Some(b'f') => (Kind::Bool, self.literal("false")?),
            Some(b'n') => (Kind::Null, self.literal("null")?),
            Some(b'-' | b'0'..=b'9') => (Kind::Num, self.number()?),
            Some(_) => return self.err("Valor inesperado"),
        };
        let id = self.nodes.len() as NodeId;
        let (depth, index) = if parent == NONE {
            (0, 0)
        } else {
            let p = &mut self.nodes[parent as usize];
            p.children.push(id);
            (p.depth + 1, (p.children.len() - 1) as u32)
        };
        self.nodes.push(Node { kind, parent, index, depth, key, text, children: Vec::new() });
        if kind.is_container() {
            self.stack.push(id);
        }
        Ok(())
    }

    fn literal(&mut self, word: &'static str) -> PResult<(u32, u32)> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(self.store(word))
        } else {
            self.err("Valor inesperado")
        }
    }

    fn number(&mut self) -> PResult<(u32, u32)> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' => self.pos += 1,
                _ => break,
            }
        }
        let raw = &self.src[start..self.pos];
        if !raw.bytes().any(|b| b.is_ascii_digit()) {
            return self.err_at(start, "Número inválido");
        }
        Ok(self.store(raw))
    }

    /// Lee una cadena; `self.pos` apunta a la comilla inicial.
    fn string(&mut self) -> PResult<Cow<'a, str>> {
        let open = self.pos;
        self.pos += 1;
        let start = self.pos;
        // Camino rápido: sin escapes.
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err_at(open, "Cadena sin cerrar"),
                Some(b'"') => {
                    let s = &self.src[start..self.pos];
                    self.pos += 1;
                    return Ok(Cow::Borrowed(s));
                }
                Some(b'\\') => break,
                Some(_) => self.pos += 1,
            }
        }
        let mut out = String::from(&self.src[start..self.pos]);
        let mut run = self.pos;
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err_at(open, "Cadena sin cerrar"),
                Some(b'"') => {
                    out.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    return Ok(Cow::Owned(out));
                }
                Some(b'\\') => {
                    out.push_str(&self.src[run..self.pos]);
                    let esc_pos = self.pos;
                    self.pos += 1;
                    let Some(e) = self.peek() else { return self.err_at(open, "Cadena sin cerrar") };
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4(esc_pos)?;
                            let c = if (0xD800..0xDC00).contains(&hi) && self.s[self.pos..].starts_with(b"\\u") {
                                let save = self.pos;
                                self.pos += 2;
                                let lo = self.hex4(esc_pos)?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                                } else {
                                    self.pos = save;
                                    None
                                }
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(c.unwrap_or('\u{FFFD}'));
                        }
                        _ => return self.err_at(esc_pos, "Secuencia de escape inválida"),
                    }
                    run = self.pos;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn hex4(&mut self, esc_pos: usize) -> PResult<u32> {
        let Some(h) = self.s.get(self.pos..self.pos + 4) else {
            return self.err_at(esc_pos, "Escape \\u incompleto");
        };
        let Ok(v) = u32::from_str_radix(std::str::from_utf8(h).unwrap_or("x"), 16) else {
            return self.err_at(esc_pos, "Escape \\u inválido");
        };
        self.pos += 4;
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Escritura

/// Tipo de token, usado para colorear.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Key,
    Str,
    Num,
    Bool,
    Null,
    Punct,
    Plain,
}

impl Tok {
    pub fn of_scalar(kind: Kind) -> Tok {
        match kind {
            Kind::Str => Tok::Str,
            Kind::Num => Tok::Num,
            Kind::Bool => Tok::Bool,
            Kind::Null => Tok::Null,
            _ => Tok::Punct,
        }
    }
}

pub trait Sink {
    fn push(&mut self, s: &str, tok: Tok);
    fn len(&self) -> usize;
}

impl Sink for String {
    fn push(&mut self, s: &str, _tok: Tok) {
        self.push_str(s);
    }
    fn len(&self) -> usize {
        String::len(self)
    }
}

/// Solo cuenta bytes (para medir el ancho de una línea compacta).
#[derive(Default)]
pub struct Counter(pub usize);

impl Sink for Counter {
    fn push(&mut self, s: &str, _tok: Tok) {
        self.0 += s.len();
    }
    fn len(&self) -> usize {
        self.0
    }
}

pub fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_str_tok<S: Sink>(s: &str, tok: Tok, sink: &mut S) {
    let mut buf = String::with_capacity(s.len() + 2);
    write_escaped(s, &mut buf);
    sink.push(&buf, tok);
}

pub fn write_key<S: Sink>(key: &str, quote: bool, sink: &mut S) {
    if quote {
        push_str_tok(key, Tok::Key, sink);
    } else {
        sink.push(key, Tok::Key);
    }
}

pub fn write_scalar<S: Sink>(doc: &Doc, n: &Node, sink: &mut S) {
    match n.kind {
        Kind::Str => push_str_tok(doc.text_of(n), Tok::Str, sink),
        Kind::Object => sink.push("{}", Tok::Punct),
        Kind::Array => sink.push("[]", Tok::Punct),
        k => sink.push(doc.text_of(n), Tok::of_scalar(k)),
    }
}

#[derive(Clone, Copy)]
pub enum Layout<'a> {
    /// Todo en una línea: `{ "a": 1, "b": [1, 2] }`
    Compact,
    /// Una línea por elemento con la sangría dada.
    Pretty(&'a str),
    /// Sin espacios.
    Minified,
}

/// Escribe el valor de `id` (sin su clave). Devuelve `false` si se cortó por `limit`.
pub fn write_value<S: Sink>(doc: &Doc, id: NodeId, layout: Layout, limit: usize, sink: &mut S) -> bool {
    struct Frame {
        id: NodeId,
        next: usize,
    }
    let mut stack: Vec<Frame> = Vec::new();

    let open = |id: NodeId, sink: &mut S, stack: &mut Vec<Frame>| {
        let n = doc.node(id);
        if n.kind.is_container() && !n.children.is_empty() {
            sink.push(if n.kind == Kind::Object { "{" } else { "[" }, Tok::Punct);
            stack.push(Frame { id, next: 0 });
        } else {
            write_scalar(doc, n, sink);
        }
    };

    let newline = |sink: &mut S, level: usize| {
        if let Layout::Pretty(indent) = layout {
            sink.push("\n", Tok::Plain);
            if level > 0 {
                sink.push(&indent.repeat(level), Tok::Plain);
            }
        }
    };

    open(id, sink, &mut stack);
    while let Some(top) = stack.last_mut() {
        if sink.len() > limit {
            sink.push(" …", Tok::Plain);
            return false;
        }
        let n = doc.node(top.id);
        let is_obj = n.kind == Kind::Object;
        if top.next == n.children.len() {
            stack.pop();
            match layout {
                Layout::Pretty(_) => newline(sink, stack.len()),
                Layout::Compact if is_obj => sink.push(" ", Tok::Plain),
                _ => {}
            }
            sink.push(if is_obj { "}" } else { "]" }, Tok::Punct);
            continue;
        }
        if top.next > 0 {
            sink.push(",", Tok::Punct);
            if let Layout::Compact = layout {
                sink.push(" ", Tok::Plain);
            }
        } else if let (Layout::Compact, true) = (layout, is_obj) {
            sink.push(" ", Tok::Plain);
        }
        let child = n.children[top.next];
        top.next += 1;
        let level = stack.len();
        newline(sink, level);
        let c = doc.node(child);
        if let Some(k) = doc.key_of(c) {
            write_key(k, true, sink);
            sink.push(":", Tok::Punct);
            if !matches!(layout, Layout::Minified) {
                sink.push(" ", Tok::Plain);
            }
        }
        open(child, sink, &mut stack);
    }
    true
}

pub fn to_string(doc: &Doc, id: NodeId, layout: Layout) -> String {
    let mut s = String::new();
    write_value(doc, id, layout, usize::MAX, &mut s);
    s
}

/// Longitud en bytes de la forma compacta, o `None` si supera `limit`.
pub fn compact_len(doc: &Doc, id: NodeId, limit: usize) -> Option<usize> {
    let mut c = Counter::default();
    write_value(doc, id, Layout::Compact, limit, &mut c).then_some(c.0).filter(|&l| l <= limit)
}

// ---------------------------------------------------------------------------
// Lexer de líneas (para colorear texto ya formateado)

/// Divide una línea de JSON en tokens `(rango, tipo)`.
pub fn lex_line(line: &str, mut f: impl FnMut(std::ops::Range<usize>, Tok)) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let tok = match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                let mut j = i;
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                if j < b.len() && b[j] == b':' { Tok::Key } else { Tok::Str }
            }
            b'-' | b'0'..=b'9' => {
                while i < b.len() && matches!(b[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    i += 1;
                }
                Tok::Num
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                i += 1;
                Tok::Punct
            }
            c if c.is_ascii_alphabetic() => {
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                match &line[start..i] {
                    "true" | "false" => Tok::Bool,
                    "null" => Tok::Null,
                    _ => Tok::Plain,
                }
            }
            _ => {
                i += 1;
                while i < b.len()
                    && !matches!(b[i], b'"' | b'-' | b'0'..=b'9' | b'{' | b'}' | b'[' | b']' | b',' | b':')
                    && !b[i].is_ascii_alphabetic()
                {
                    i += 1;
                }
                Tok::Plain
            }
        };
        // No partir caracteres UTF-8.
        while i < b.len() && !line.is_char_boundary(i) {
            i += 1;
        }
        f(start..i, tok);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonc() {
        let src = r#"{
            // comentario
            "Esperado": { "Exito": true, "n": 15.000000, /* x */ "a": [1, 2,], },
            "s": "a\"bé😀"
        }"#;
        let doc = parse(src).unwrap();
        assert_eq!(
            to_string(&doc, 0, Layout::Minified),
            r#"{"Esperado":{"Exito":true,"n":15.000000,"a":[1,2]},"s":"a\"bé😀"}"#
        );
        assert_eq!(to_string(&doc, 1, Layout::Compact), r#"{ "Exito": true, "n": 15.000000, "a": [1, 2] }"#);
        assert_eq!(doc.path(4), "$.Esperado.a");
        assert_eq!(doc.subtree_end(1), 7);
    }

    #[test]
    fn reports_error_position() {
        let e = parse("{\n  \"a\": 1\n  \"b\": 2\n}").err().unwrap();
        assert_eq!((e.line, e.col), (3, 3));
    }

    #[test]
    fn filters_by_keys() {
        let doc = parse(
            r#"{"Encabezado":{"U_SO1_AUTORIZADO":"N","Otro":1},"Partidas":[{"Codigo":"A","x":1},{"y":2},{"codigo":{"z":[1]}}]}"#,
        )
        .unwrap();
        let f = doc.filter_keys(&["u_so1_autorizado", "Codigo", "NoExiste"]);
        assert_eq!(
            to_string(&f.doc, 0, Layout::Minified),
            r#"{"Encabezado":{"U_SO1_AUTORIZADO":"N"},"Partidas":[{"Codigo":"A"},{"codigo":{"z":[1]}}]}"#
        );
        assert_eq!(f.counts, vec![1, 2, 0]);
        assert_eq!(f.path, vec![true, true, false, true, true, false, true, false, false, false]);
        let none = doc.filter_keys(&["NoExiste"]);
        assert_eq!(to_string(&none.doc, 0, Layout::Minified), "{}");
    }

    #[test]
    fn pretty() {
        let doc = parse(r#"{"a":[1,{}],"b":{"c":null}}"#).unwrap();
        assert_eq!(
            to_string(&doc, 0, Layout::Pretty("  ")),
            "{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": {\n    \"c\": null\n  }\n}"
        );
    }
}
