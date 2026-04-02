// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License version 3
// as published by the Free Software Foundation.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextValue {
    pub data: ContextData,
    pub metadata: ContextMetadata,
}

#[derive(Debug, Clone)]
pub enum ContextData {
    Text(String),
    Json(serde_json::Value),
    Binary(Vec<u8>),
}

impl Serialize for ContextData {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            ContextData::Text(s) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "text")?;
                map.serialize_entry("content", s)?;
                map.end()
            }
            ContextData::Json(v) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "json")?;
                map.serialize_entry("content", v)?;
                map.end()
            }
            ContextData::Binary(bytes) => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("type", "binary")?;
                map.serialize_entry("content", &BASE64.encode(bytes))?;
                map.serialize_entry("encoding", "base64")?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ContextData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or(serde::de::Error::custom("expected object"))?;
        let data_type = obj
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or(serde::de::Error::custom("missing type field"))?;

        match data_type {
            "text" => {
                let content = obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or(serde::de::Error::custom("missing content"))?;
                Ok(ContextData::Text(content.to_string()))
            }
            "json" => {
                let content = obj
                    .get("content")
                    .ok_or(serde::de::Error::custom("missing content"))?;
                Ok(ContextData::Json(content.clone()))
            }
            "binary" => {
                let content = obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or(serde::de::Error::custom("missing content"))?;
                let bytes = BASE64.decode(content).map_err(serde::de::Error::custom)?;
                Ok(ContextData::Binary(bytes))
            }
            other => Err(serde::de::Error::custom(format!("unknown type: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    pub source: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub name: String,
    pub path: String,
    pub entry_type: EntryType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    File,
    Directory,
    Table,
    Row,
}
