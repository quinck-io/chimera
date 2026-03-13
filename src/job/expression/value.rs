#[derive(Debug, Clone)]
pub(crate) enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => *n != 0.0,
            Value::Null => false,
            // Arrays and objects are always truthy (matches GitHub behavior)
            Value::Array(_) | Value::Object(_) => true,
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
            Value::Array(_) | Value::Object(_) => {
                serde_json::to_string(&self.to_json()).unwrap_or_default()
            }
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Value::String(s) => serde_json::Value::String(s.clone()),
            Value::Number(n) => serde_json::json!(*n),
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Null => serde_json::Value::Null,
            Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            Value::Object(pairs) => {
                let map = pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect();
                serde_json::Value::Object(map)
            }
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
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y)
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((k1, v1), (k2, v2))| k1.eq_ignore_ascii_case(k2) && v1 == v2)
            }
            _ => self.to_display().eq_ignore_ascii_case(&other.to_display()),
        }
    }
}
