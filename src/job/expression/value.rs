#[derive(Debug, Clone)]
pub(crate) enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

impl Value {
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => *n != 0.0,
            Value::Null => false,
        }
    }

    pub(crate) fn to_display(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Number(n) => {
                if *n == (*n as i64) as f64 {
                    (*n as i64).to_string()
                } else {
                    n.to_string()
                }
            }
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a.eq_ignore_ascii_case(b),
            _ => self.to_display().eq_ignore_ascii_case(&other.to_display()),
        }
    }
}
