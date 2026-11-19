use std::borrow::Cow;
use std::collections::BTreeMap;
use std::range::Range;

use toml_edit::{Array, Document, InlineTable, Item, Table, TableLike, Value};
use typst::syntax::FileId;

use crate::check::Diagnostics;
use crate::check::codes::Code;
use crate::check::diagnostics::{
    Diagnostic, Docs, EmittableDiagnostic, EmittableResult, Label, MaybeEmit, Result,
};

impl Diagnostic {
    /// Emit a diagnostic at a toml manifest key.
    pub fn emit_at<V>(self, parser: &mut ValueParser<Spanned<V>>) {
        parser.diags.emit(parser.enrich(self));
    }
}

pub fn file<'d, 'v>(
    diags: &'d mut Diagnostics,
    file_id: FileId,
    doc: &'v Document<&str>,
) -> File<'d, 'v> {
    File {
        diags,
        file_id,
        doc,
    }
}

pub struct File<'d, 'v> {
    diags: &'d mut Diagnostics,
    file_id: FileId,
    doc: &'v Document<&'v str>,
}

impl<'d, 'v> File<'d, 'v> {
    pub fn require_field<'slf>(
        &'slf mut self,
        key: &'static str,
        missing: Code,
    ) -> EmittableResult<'slf, ValueParser<'slf, Spanned<&'v Item>>> {
        let Some(item) = self.doc.get_spanned(key) else {
            let diag = Diagnostic::error(missing)
                .with_message(format!("Missing required `[{key}]` table"))
                .with_label(Label::new(self.file_id, 0..0))
                .with_docs_link(Docs::MANIFEST);
            return Err(EmittableDiagnostic::new(self.diags, diag));
        };
        Ok(ValueParser {
            diags: self.diags,
            path: TomlPath {
                parent: Parent::File(self.file_id),
                key: key.into(),
            },
            item,
        })
    }

    pub fn optional_field<'slf>(
        &'slf mut self,
        key: &'static str,
    ) -> Option<ValueParser<'slf, Spanned<&'v Item>>> {
        let item = self.doc.get_spanned(key)?;
        Some(ValueParser {
            diags: self.diags,
            path: TomlPath {
                parent: Parent::File(self.file_id),
                key: key.into(),
            },
            item,
        })
    }
}

pub type TableLikeParser<'d, 'v, T> = ValueParser<'d, Spanned<&'v T>>;
pub type TableParser<'d, 'v> = TableLikeParser<'d, 'v, Table>;
pub type ArrayParser<'d, 'v> = ValueParser<'d, Spanned<&'v Array>>;

pub struct ValueParser<'d, T> {
    diags: &'d mut Diagnostics,
    path: TomlPath<'d>,
    pub item: T,
}

impl<'d, T> ValueParser<'d, Spanned<T>> {
    /// Add information related to this field to the diagnostic.
    fn enrich(&self, mut diag: Diagnostic) -> Diagnostic {
        if diag.label.is_none() {
            diag = diag.with_label(Label::new(self.path.file_id(), self.item.span()));
        }
        if !diag.has_docs_links() {
            diag = diag.with_docs_link(Docs::for_manifest_path(&self.path))
        }
        diag
    }

    fn take_value(self) -> (ValueParser<'d, Spanned<()>>, T) {
        let parser = ValueParser {
            diags: self.diags,
            path: self.path,
            item: Spanned::new((), self.item.span),
        };
        (parser, self.item.val)
    }

    fn with_value<V>(self, val: V) -> ValueParser<'d, Spanned<V>> {
        self.map(|_| val)
    }

    fn map<F, V>(self, map: F) -> ValueParser<'d, Spanned<V>>
    where
        F: FnOnce(T) -> V,
    {
        ValueParser {
            diags: self.diags,
            path: self.path,
            item: self.item.map(map),
        }
    }

    fn child<'slf, 'v, V>(
        &'slf mut self,
        key: impl Into<TomlKey>,
        item: Spanned<&'v V>,
    ) -> ValueParser<'slf, Spanned<&'v V>> {
        ValueParser {
            diags: self.diags,
            path: self.path.join(key),
            item,
        }
    }
}

impl<'d, 'v, T: TableLike> TableLikeParser<'d, 'v, T> {
    pub fn require_field<'slf>(
        &'slf mut self,
        key: &'static str,
        missing: Code,
    ) -> EmittableResult<'slf, ValueParser<'slf, Spanned<&'v Item>>> {
        let path = self.path.join(key);
        let Some(item) = self.item.get_spanned(key) else {
            let diag = self.enrich(
                Diagnostic::error(missing).with_message(format!("Missing required field `{path}`")),
            );
            return Err(EmittableDiagnostic::new(self.diags, diag));
        };
        Ok(self.child(key, item))
    }

    pub fn required_field<'slf>(
        &'slf mut self,
        key: &'static str,
        missing: Code,
    ) -> Option<ValueParser<'slf, Spanned<&'v Item>>> {
        self.require_field(key, missing).maybe_emit()
    }

    pub fn optional_field<'slf>(
        &'slf mut self,
        key: &'static str,
    ) -> Option<ValueParser<'slf, Spanned<&'v Item>>> {
        let item = self.item.get_spanned(key)?;
        Some(self.child(key, item))
    }

    pub fn unexpected<const N: usize>(&mut self, unexpected: Code, expected: [&'static str; N]) {
        let table = self.item.val;
        for (key, _) in table.iter() {
            if !expected.contains(&key) {
                let key_span = table.key(key).unwrap().span().unwrap();
                Diagnostic::warn(unexpected.clone())
                    .with_message("unexpected key")
                    .with_label(Label::new(self.path.file_id(), key_span))
                    .emit_at(self);
            }
        }
    }

    pub fn parse_map<K: Ord, V, FK, FV>(
        self,
        mut parse_key: FK,
        mut parse_value: FV,
    ) -> ValueParser<'d, Spanned<BTreeMap<K, Spanned<V>>>>
    where
        FK: FnMut(&'v str) -> Result<K>,
        FV: for<'a> FnMut(Spanned<&'a K>, ValueParser<Spanned<&'v Item>>) -> Result<Spanned<V>>,
    {
        let (mut parser, table) = self.take_value();
        let items = table
            .iter()
            .filter_map(|(key, item)| {
                let key_span = Range::from(table.key(key).unwrap().span().unwrap());
                let parsed_key = match parse_key(key) {
                    Ok(key) => key,
                    Err(mut diag) => {
                        if diag.label.is_none() {
                            diag = diag.with_label(Label::new(parser.path.file_id(), key_span));
                        }
                        diag.emit_at(&mut parser);
                        return None;
                    }
                };

                let item_span = Range::from(item.span().unwrap());
                let item = Spanned::new(item, item_span);
                let child = parser.child(TomlKey::Str(Cow::Owned(key.to_string())), item);
                let parsed_item = match parse_value(Spanned::new(&parsed_key, key_span), child) {
                    Ok(val) => val,
                    Err(mut diag) => {
                        if diag.label.is_none() {
                            diag = diag.with_label(Label::new(parser.path.file_id(), item_span));
                        }
                        diag.emit_at(&mut parser);
                        return None;
                    }
                };

                Some((parsed_key, parsed_item))
            })
            .collect();
        parser.map(|_| items)
    }
}

impl<'d, 'v> ArrayParser<'d, 'v> {
    pub fn parse_array<V, F>(self, mut parse: F) -> ValueParser<'d, Spanned<Vec<Spanned<V>>>>
    where
        F: FnMut(ValueParser<Spanned<&'v Value>>) -> Result<Spanned<V>>,
    {
        let (mut parser, array) = self.take_value();
        let items = array
            .iter()
            .enumerate()
            .filter_map(|(idx, value)| {
                let item_span = Range::from(value.span().unwrap());
                let item = Spanned::new(value, item_span);
                let child = parser.child(idx, item);
                match parse(child) {
                    Ok(val) => Some(val),
                    Err(mut diag) => {
                        if diag.label.is_none() {
                            diag = diag.with_label(Label::new(parser.path.file_id(), item_span));
                        }
                        diag.emit_at(&mut parser);
                        None
                    }
                }
            })
            .collect();
        parser.map(|_| items)
    }
}

impl<'d, T> ValueParser<'d, Spanned<T>> {
    pub fn validate<F>(self, validate: F) -> Self
    where
        F: for<'t> FnOnce(&mut ValueParser<'t, Spanned<()>>, &T),
    {
        let (mut parser, item) = self.take_value();
        validate(&mut parser, &item);
        parser.with_value(item)
    }

    pub fn parse_with<F, V>(self, parse: F) -> EmittableResult<'d, ValueParser<'d, Spanned<V>>>
    where
        F: for<'t> FnOnce(&mut ValueParser<'t, Spanned<()>>, T) -> Result<V>,
    {
        let (mut parser, val) = self.take_value();
        match parse(&mut parser, val) {
            Ok(val) => Ok(parser.with_value(val)),
            Err(diag) => {
                let diag = parser.enrich(diag);
                Err(EmittableDiagnostic::new(parser.diags, diag))
            }
        }
    }

    pub fn parse<F, V>(self, parse: F) -> EmittableResult<'d, ValueParser<'d, Spanned<V>>>
    where
        F: FnOnce(T) -> Result<V>,
    {
        self.parse_with(|_, val| parse(val))
    }

    pub fn parsed_with<F, V>(self, parse: F) -> Option<ValueParser<'d, Spanned<V>>>
    where
        F: for<'t> FnOnce(&mut ValueParser<'t, Spanned<()>>, T) -> Result<V>,
    {
        self.parse_with(parse).maybe_emit()
    }

    pub fn parsed<F, V>(self, parse: F) -> Option<ValueParser<'d, Spanned<V>>>
    where
        F: FnOnce(T) -> Result<V>,
    {
        self.parse(parse).maybe_emit()
    }

    fn expect_type<V, F>(
        self,
        ty: Code,
        what: &'static str,
        expect: F,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<V>>>
    where
        F: FnOnce(T) -> Option<V>,
    {
        self.parse_with(|parser, item| {
            expect(item).ok_or_else(|| {
                Diagnostic::error(ty).with_message(format!("`{}` should be {what}", parser.path))
            })
        })
    }
}

impl<'d, 'v> ValueParser<'d, Spanned<&'v Item>> {
    pub fn expect_table(
        self,
        ty: Code,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Table>>> {
        self.expect_type(ty, "a table", Item::as_table)
    }

    pub fn expected_table(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v Table>>> {
        self.expect_table(ty).maybe_emit()
    }

    pub fn expect_inline_table(
        self,
        ty: Code,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v InlineTable>>> {
        self.expect_type(ty, "an inline table", Item::as_inline_table)
    }

    pub fn expect_array(
        self,
        ty: Code,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Array>>> {
        self.expect_type(ty, "an array", Item::as_array)
    }

    pub fn expected_array(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v Array>>> {
        self.expect_array(ty).maybe_emit()
    }

    pub fn expect_str(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v str>>> {
        self.expect_type(ty, "a string", Item::as_str)
    }

    pub fn expected_str(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v str>>> {
        self.expect_str(ty).maybe_emit()
    }

    pub fn expect_bool(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<bool>>> {
        self.expect_type(ty, "a boolean", Item::as_bool)
    }
}

impl<'d, 'v> ValueParser<'d, Spanned<&'v Value>> {
    pub fn expect_array(
        self,
        ty: Code,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Array>>> {
        self.expect_type(ty, "an array", Value::as_array)
    }

    pub fn expect_str(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v str>>> {
        self.expect_type(ty, "a string", Value::as_str)
    }
}

pub struct TomlPath<'p> {
    parent: Parent<'p>,
    pub key: TomlKey,
}

enum Parent<'p> {
    File(FileId),
    Path(&'p TomlPath<'p>),
}

impl<'p> TomlPath<'p> {
    pub fn join<'slf>(&'slf self, key: impl Into<TomlKey>) -> TomlPath<'slf> {
        TomlPath {
            parent: Parent::Path(self),
            key: key.into(),
        }
    }

    pub fn file_id(&self) -> FileId {
        let mut path = self;
        loop {
            match path.parent {
                Parent::File(file_id) => return file_id,
                Parent::Path(p) => path = p,
            }
        }
    }

    pub fn root(&self) -> &str {
        let mut path = self;
        while let Parent::Path(parent) = path.parent {
            path = parent;
        }
        // Can't have a root array.
        path.key.str().unwrap()
    }
}

impl std::fmt::Display for TomlPath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Parent::Path(parent) = self.parent {
            parent.fmt(f)?;
            // If the child is an array index omit the dot: `<parent>[<idx>]`.
            if matches!(self.key, TomlKey::Str(_)) {
                f.write_str(".")?;
            }
        }
        self.key.fmt(f)
    }
}

pub enum TomlKey {
    Str(Cow<'static, str>),
    Idx(usize),
}

impl From<usize> for TomlKey {
    fn from(v: usize) -> Self {
        Self::Idx(v)
    }
}

impl From<&'static str> for TomlKey {
    fn from(v: &'static str) -> Self {
        Self::Str(Cow::Borrowed(v))
    }
}

impl TomlKey {
    pub fn str(&self) -> Option<&str> {
        if let Self::Str(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

impl std::fmt::Display for TomlKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TomlKey::Str(str) => {
                if str
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-'))
                {
                    f.write_str(str)
                } else {
                    write!(f, "{str:?}")
                }
            }
            TomlKey::Idx(idx) => write!(f, "[{idx}]"),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Spanned<T> {
    pub val: T,
    span: Range<usize>,
}

impl<T> Spanned<T> {
    pub fn new(val: T, span: impl Into<Range<usize>>) -> Self {
        Self {
            val,
            span: span.into(),
        }
    }

    pub fn span(&self) -> Range<usize> {
        self.span
    }

    pub fn as_ref(&self) -> Spanned<&T> {
        Spanned {
            val: &self.val,
            span: self.span,
        }
    }

    pub fn map<F, V>(self, f: F) -> Spanned<V>
    where
        F: FnOnce(T) -> V,
    {
        Spanned {
            val: f(self.val),
            span: self.span,
        }
    }

    pub fn filter_map<F, V>(self, f: F) -> Option<Spanned<V>>
    where
        F: FnOnce(T) -> Option<V>,
    {
        Some(Spanned {
            val: f(self.val)?,
            span: self.span,
        })
    }

    pub fn try_map<F, V, E>(self, f: F) -> std::result::Result<Spanned<V>, E>
    where
        F: FnOnce(T) -> std::result::Result<V, E>,
    {
        Ok(Spanned {
            val: f(self.val)?,
            span: self.span,
        })
    }
}

impl<T: ?Sized + ToOwned> Spanned<&T> {
    pub fn to_owned(self) -> Spanned<T::Owned>
    where
        T: ToOwned,
    {
        Spanned {
            val: self.val.to_owned(),
            span: self.span,
        }
    }
}

impl<T> AsRef<T> for Spanned<T> {
    fn as_ref(&self) -> &T {
        &self.val
    }
}

impl<T> std::ops::Deref for Spanned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.val
    }
}

impl<T> std::ops::DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.val
    }
}

trait TomlExt {
    type Item;

    fn get_spanned(&self, name: &'static str) -> Option<Spanned<&Self::Item>>;
}

impl<T: TableLike> TomlExt for T {
    type Item = Item;

    fn get_spanned(&self, name: &'static str) -> Option<Spanned<&Self::Item>> {
        let item = self.get(name)?;
        // The span should only ever be none for user-created tables, not
        // deserialized ones.
        let span = item.span().unwrap();
        Some(Spanned::new(item, span))
    }
}
