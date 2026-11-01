//! Macros used by the generated API modules.
//!
//! bunny.net enums are open-ended: new values appear without notice. Every
//! enum therefore has an `Other(..)` variant, so an unknown value never makes
//! a response fail to decode.

use serde::Deserialize;

/// An enum value as sent by the API: usually a number, sometimes its name.
#[derive(Deserialize)]
#[serde(untagged)]
#[doc(hidden)]
pub enum IntOrString {
    Int(i64),
    Str(String),
}

/// Compares an API enum name with a Rust variant name, ignoring case and
/// separators (`full-access` matches `FullAccess`).
#[doc(hidden)]
pub fn same_name(api: &str, variant: &str) -> bool {
    let mut a = api.chars().filter(char::is_ascii_alphanumeric);
    let mut b = variant.chars().filter(char::is_ascii_alphanumeric);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x.eq_ignore_ascii_case(&y) => {}
            _ => return false,
        }
    }
}

/// An enum sent as an integer.
macro_rules! int_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $value:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this version of the SDK does not know.
            Other(i64),
        }

        impl $name {
            /// The integer sent over the wire.
            pub fn value(self) -> i64 {
                match self {
                    $( Self::$variant => $value, )+
                    Self::Other(value) => value,
                }
            }

            /// The variant for an integer from the API.
            pub fn from_value(value: i64) -> Self {
                match value {
                    $( $value => Self::$variant, )+
                    other => Self::Other(other),
                }
            }

            fn from_name(name: &str) -> ::core::option::Option<Self> {
                $( if $crate::macros::same_name(name, stringify!($variant)) {
                    return ::core::option::Option::Some(Self::$variant);
                } )+
                ::core::option::Option::None
            }
        }

        impl ::core::default::Default for $name {
            fn default() -> Self {
                [$( Self::$variant ),+][0]
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::Display::fmt(&self.value(), f)
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                serializer.serialize_i64(self.value())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                match <$crate::macros::IntOrString as ::serde::Deserialize>::deserialize(deserializer)? {
                    $crate::macros::IntOrString::Int(value) => Ok(Self::from_value(value)),
                    $crate::macros::IntOrString::Str(name) => name
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .map(Self::from_value)
                        .or_else(|| Self::from_name(&name))
                        .ok_or_else(|| {
                            <D::Error as ::serde::de::Error>::custom(format!(
                                "unknown {} value {name:?}",
                                stringify!($name)
                            ))
                        }),
                }
            }
        }
    };
}

/// An enum sent as a string.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $value:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this version of the SDK does not know.
            Other(::std::string::String),
        }

        impl $name {
            /// The string sent over the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $value, )+
                    Self::Other(value) => value,
                }
            }

            /// The variant for a string from the API (case-insensitive).
            pub fn from_value(value: &str) -> Self {
                $( if value.eq_ignore_ascii_case($value) {
                    return Self::$variant;
                } )+
                Self::Other(value.to_owned())
            }
        }

        impl ::core::default::Default for $name {
            fn default() -> Self {
                [$( Self::$variant ),+].into_iter().next().expect("enum has variants")
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                let value = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                Ok(Self::from_value(&value))
            }
        }
    };
}

#[cfg(test)]
mod tests {
    int_enum! {
        /// Test enum.
        Kind {
            None = 0,
            /// Allow it.
            Allow = 1,
            BlockAll = 5,
        }
    }

    str_enum! {
        Permission {
            FullAccess = "full-access",
            ReadOnly = "read-only",
        }
    }

    #[test]
    fn int_enum_round_trips_and_tolerates_unknown_values() {
        assert_eq!(serde_json::to_string(&Kind::BlockAll).unwrap(), "5");
        assert_eq!(serde_json::from_str::<Kind>("1").unwrap(), Kind::Allow);
        assert_eq!(serde_json::from_str::<Kind>("42").unwrap(), Kind::Other(42));
        assert_eq!(serde_json::to_string(&Kind::Other(42)).unwrap(), "42");
        assert_eq!(
            serde_json::from_str::<Kind>("\"block_all\"").unwrap(),
            Kind::BlockAll
        );
        assert_eq!(
            serde_json::from_str::<Kind>("\"5\"").unwrap(),
            Kind::BlockAll
        );
        assert!(serde_json::from_str::<Kind>("\"nope\"").is_err());
        assert_eq!(Kind::default(), Kind::None);
        assert_eq!(Kind::Allow.to_string(), "1");
    }

    #[test]
    fn str_enum_round_trips_and_tolerates_unknown_values() {
        assert_eq!(
            serde_json::to_string(&Permission::ReadOnly).unwrap(),
            "\"read-only\""
        );
        assert_eq!(
            serde_json::from_str::<Permission>("\"FULL-ACCESS\"").unwrap(),
            Permission::FullAccess
        );
        assert_eq!(
            serde_json::from_str::<Permission>("\"admin\"").unwrap(),
            Permission::Other("admin".into())
        );
        assert_eq!(Permission::default(), Permission::FullAccess);
        assert_eq!(Permission::ReadOnly.to_string(), "read-only");
    }
}
