//! Structural representation used to inspect a frozen v1–v3 verifier without executing it.
//! The node fields follow the RustPython 0.4 ASDL schema; source locations are intentionally absent.
use rustpython_parser::{ast, Parse};
use serde_json::{json, Value};

pub struct RustAstPort;
impl crate::verifier_parity::VerifierAstPort for RustAstPort {
    fn parse(&self, source: &[u8]) -> Result<Value, String> {
        if source.len() > 2 * 1024 * 1024 {
            return Err("Oversized historical verifier".into());
        }
        let text =
            std::str::from_utf8(source).map_err(|_| "Invalid historical verifier encoding")?;
        let tree = ast::Suite::parse(text, "<historical-verifier>")
            .map_err(|_| "Invalid historical verifier syntax")?;
        Ok(json!({"_type":"Module","body":tree.value(),"type_ignores":[]}))
    }
}

trait AstValue {
    fn value(&self) -> Value;
}
impl<T: AstValue> AstValue for Box<T> {
    fn value(&self) -> Value {
        (**self).value()
    }
}
impl<T: AstValue> AstValue for Option<T> {
    fn value(&self) -> Value {
        self.as_ref().map_or(Value::Null, AstValue::value)
    }
}
impl<T: AstValue> AstValue for Vec<T> {
    fn value(&self) -> Value {
        Value::Array(self.iter().map(AstValue::value).collect())
    }
}
impl AstValue for String {
    fn value(&self) -> Value {
        json!(self)
    }
}
impl AstValue for ast::Identifier {
    fn value(&self) -> Value {
        json!(self.as_str())
    }
}
impl AstValue for ast::Int {
    fn value(&self) -> Value {
        json!(self.to_u32())
    }
}
impl AstValue for bool {
    fn value(&self) -> Value {
        json!(self)
    }
}
impl AstValue for ast::ConversionFlag {
    fn value(&self) -> Value {
        json!(*self as i8)
    }
}
fn float_repr(value: f64) -> String {
    let shortest = format!("{value:?}");
    match shortest.split_once('e') {
        Some((digits, power)) => match power.parse::<i32>() {
            Ok(exponent) => format!("{digits}e{exponent:+03}"),
            Err(_) => shortest,
        },
        None => shortest,
    }
}
impl AstValue for ast::Constant {
    fn value(&self) -> Value {
        match self {
            Self::None => Value::Null,
            Self::Bool(b) => json!(b),
            Self::Str(s) => json!(s),
            Self::Int(i) => serde_json::from_str(&i.to_string())
                .unwrap_or_else(|_| json!({"$int":i.to_string()})),
            Self::Float(f) => json!({"$float":float_repr(*f)}),
            Self::Complex { .. } => json!({"$other":"complex"}),
            Self::Tuple(_) => json!({"$other":"tuple"}),
            Self::Ellipsis => json!({"$other":"ellipsis"}),
            Self::Bytes(bytes) => {
                json!({"$bytes":bytes.iter().map(|b|format!("{b:02x}")).collect::<String>()})
            }
        }
    }
}

// Historical parity compares structural fields and deliberately omits source positions.
macro_rules! variants {
    ($name:ident, $($variant:ident),+ $(,)?) => {
        impl AstValue for ast::$name {
            fn value(&self) -> Value {
                match self {$(Self::$variant(node) => node.value(),)+}
            }
        }
    };
}
variants!(
    Stmt,
    FunctionDef,
    AsyncFunctionDef,
    ClassDef,
    Return,
    Delete,
    Assign,
    TypeAlias,
    AugAssign,
    AnnAssign,
    For,
    AsyncFor,
    While,
    If,
    With,
    AsyncWith,
    Match,
    Raise,
    Try,
    TryStar,
    Assert,
    Import,
    ImportFrom,
    Global,
    Nonlocal,
    Expr,
    Pass,
    Break,
    Continue
);
variants!(
    Expr,
    BoolOp,
    NamedExpr,
    BinOp,
    UnaryOp,
    Lambda,
    IfExp,
    Dict,
    Set,
    ListComp,
    SetComp,
    DictComp,
    GeneratorExp,
    Await,
    Yield,
    YieldFrom,
    Compare,
    Call,
    FormattedValue,
    JoinedStr,
    Constant,
    Attribute,
    Subscript,
    Starred,
    Name,
    List,
    Tuple,
    Slice
);
variants!(ExceptHandler, ExceptHandler);
variants!(
    Pattern,
    MatchValue,
    MatchSingleton,
    MatchSequence,
    MatchMapping,
    MatchClass,
    MatchStar,
    MatchAs,
    MatchOr
);
variants!(TypeIgnore, TypeIgnore);
variants!(TypeParam, TypeVar, ParamSpec, TypeVarTuple);
impl AstValue for ast::ExprContext {
    fn value(&self) -> Value {
        json!({"_type":format!("{self:?}")})
    }
}
impl AstValue for ast::BoolOp {
    fn value(&self) -> Value {
        json!({"_type":format!("{self:?}")})
    }
}
impl AstValue for ast::Operator {
    fn value(&self) -> Value {
        json!({"_type":format!("{self:?}")})
    }
}
impl AstValue for ast::UnaryOp {
    fn value(&self) -> Value {
        json!({"_type":format!("{self:?}")})
    }
}
impl AstValue for ast::CmpOp {
    fn value(&self) -> Value {
        json!({"_type":format!("{self:?}")})
    }
}
macro_rules! fields {
    ($ty:ident, $name:literal $(, $field:ident)* $(,)?) => {
        impl AstValue for ast::$ty {
            fn value(&self) -> Value {
                #[allow(unused_mut)]
                let mut fields=serde_json::Map::new();
                fields.insert("_type".into(), $name.into());
                $(fields.insert(stringify!($field).trim_start_matches("r#").into(), self.$field.value());)*
                Value::Object(fields)
            }
        }
    };
}
fields!(ModModule, "Module", body, type_ignores);
fields!(
    StmtFunctionDef,
    "FunctionDef",
    name,
    args,
    body,
    decorator_list,
    returns,
    type_comment,
    type_params
);
fields!(
    StmtAsyncFunctionDef,
    "AsyncFunctionDef",
    name,
    args,
    body,
    decorator_list,
    returns,
    type_comment,
    type_params
);
fields!(
    StmtClassDef,
    "ClassDef",
    name,
    bases,
    keywords,
    body,
    decorator_list,
    type_params
);
fields!(StmtReturn, "Return", value);
fields!(StmtDelete, "Delete", targets);
fields!(StmtAssign, "Assign", targets, value, type_comment);
fields!(StmtTypeAlias, "TypeAlias", name, type_params, value);
fields!(StmtAugAssign, "AugAssign", target, op, value);
impl AstValue for ast::StmtAnnAssign {
    fn value(&self) -> Value {
        json!({"_type":"AnnAssign","target":self.target.value(),"annotation":self.annotation.value(),"value":self.value.value(),"simple":i32::from(self.simple)})
    }
}
fields!(StmtFor, "For", target, iter, body, orelse, type_comment);
fields!(
    StmtAsyncFor,
    "AsyncFor",
    target,
    iter,
    body,
    orelse,
    type_comment
);
fields!(StmtWhile, "While", test, body, orelse);
fields!(StmtIf, "If", test, body, orelse);
fields!(StmtWith, "With", items, body, type_comment);
fields!(StmtAsyncWith, "AsyncWith", items, body, type_comment);
fields!(StmtMatch, "Match", subject, cases);
fields!(StmtRaise, "Raise", exc, cause);
fields!(StmtTry, "Try", body, handlers, orelse, finalbody);
fields!(StmtTryStar, "TryStar", body, handlers, orelse, finalbody);
fields!(StmtAssert, "Assert", test, msg);
fields!(StmtImport, "Import", names);
fields!(StmtImportFrom, "ImportFrom", module, names, level);
fields!(StmtGlobal, "Global", names);
fields!(StmtNonlocal, "Nonlocal", names);
fields!(StmtExpr, "Expr", value);
fields!(StmtPass, "Pass");
fields!(StmtBreak, "Break");
fields!(StmtContinue, "Continue");
fields!(ExprBoolOp, "BoolOp", op, values);
fields!(ExprNamedExpr, "NamedExpr", target, value);
fields!(ExprBinOp, "BinOp", left, op, right);
fields!(ExprUnaryOp, "UnaryOp", op, operand);
fields!(ExprLambda, "Lambda", args, body);
fields!(ExprIfExp, "IfExp", test, body, orelse);
fields!(ExprDict, "Dict", keys, values);
fields!(ExprSet, "Set", elts);
fields!(ExprListComp, "ListComp", elt, generators);
fields!(ExprSetComp, "SetComp", elt, generators);
fields!(ExprDictComp, "DictComp", key, value, generators);
fields!(ExprGeneratorExp, "GeneratorExp", elt, generators);
fields!(ExprAwait, "Await", value);
fields!(ExprYield, "Yield", value);
fields!(ExprYieldFrom, "YieldFrom", value);
fields!(ExprCompare, "Compare", left, ops, comparators);
fields!(ExprCall, "Call", func, args, keywords);
impl AstValue for ast::ExprFormattedValue {
    fn value(&self) -> Value {
        let v = self;
        json!({"_type":"FormattedValue","value":v.value.value(),"conversion":v.conversion.value(),"format_spec":v.format_spec.value()})
    }
}
fields!(ExprJoinedStr, "JoinedStr", values);
fields!(ExprConstant, "Constant", value, kind);
fields!(ExprAttribute, "Attribute", value, attr, ctx);
fields!(ExprSubscript, "Subscript", value, slice, ctx);
fields!(ExprStarred, "Starred", value, ctx);
fields!(ExprName, "Name", id, ctx);
fields!(ExprList, "List", elts, ctx);
fields!(ExprTuple, "Tuple", elts, ctx);
fields!(ExprSlice, "Slice", lower, upper, step);
impl AstValue for ast::Comprehension {
    fn value(&self) -> Value {
        let v = self;
        json!({"_type":"comprehension","target":v.target.value(),"iter":v.iter.value(),"ifs":v.ifs.value(),"is_async":v.is_async as u8})
    }
}
impl AstValue for ast::ExceptHandlerExceptHandler {
    fn value(&self) -> Value {
        json!({"_type":"ExceptHandler","type":self.type_.value(),"name":self.name.value(),"body":self.body.value()})
    }
}
fields!(Arg, "arg", arg, annotation, type_comment);
fields!(Keyword, "keyword", arg, value);
fields!(Alias, "alias", name, asname);
fields!(WithItem, "withitem", context_expr, optional_vars);
fields!(MatchCase, "match_case", pattern, guard, body);
fields!(PatternMatchValue, "MatchValue", value);
fields!(PatternMatchSingleton, "MatchSingleton", value);
fields!(PatternMatchSequence, "MatchSequence", patterns);
fields!(PatternMatchMapping, "MatchMapping", keys, patterns, rest);
fields!(
    PatternMatchClass,
    "MatchClass",
    cls,
    patterns,
    kwd_attrs,
    kwd_patterns
);
fields!(PatternMatchStar, "MatchStar", name);
fields!(PatternMatchAs, "MatchAs", pattern, name);
fields!(PatternMatchOr, "MatchOr", patterns);
fields!(TypeParamTypeVar, "TypeVar", name, bound);
fields!(TypeParamParamSpec, "ParamSpec", name);
fields!(TypeParamTypeVarTuple, "TypeVarTuple", name);

impl AstValue for ast::Arguments {
    fn value(&self) -> Value {
        let args = self.to_python_arguments();
        let keyword_defaults: Vec<_> = self
            .kwonlyargs
            .iter()
            .map(|arg| arg.default.value())
            .collect();
        json!({"_type":"arguments","args":args.args.value(),"posonlyargs":args.posonlyargs.value(),"kwonlyargs":args.kwonlyargs.value(),
            "defaults":args.defaults.value(),"kw_defaults":keyword_defaults,"vararg":args.vararg.value(),"kwarg":args.kwarg.value()})
    }
}

fields!(TypeIgnoreTypeIgnore, "TypeIgnore", lineno, tag);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier_parity::VerifierAstPort;
    #[test]
    fn frozen_keyword_defaults_and_handlers_preserve_structural_fields() {
        let ast = RustAstPort.parse(b"def check(*, required, limit=1e-7):\n try: pass\n except ValueError as exc: pass\n").unwrap();
        assert_eq!(
            ast["body"][0]["args"]["kw_defaults"],
            json!([null,{"_type":"Constant","kind":null,"value":{"$float":"1e-07"}}])
        );
        let handler = &ast["body"][0]["body"][0]["handlers"][0];
        assert_eq!(handler["type"]["id"], "ValueError");
        assert_eq!(handler["name"], "exc");
    }
}
