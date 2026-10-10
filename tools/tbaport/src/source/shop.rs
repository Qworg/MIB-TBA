//! `.shp` reader — mirrors `boot_the_shops` (`shop.c:1203`).
//!
//! A shop record is `#<shop vnum>~` followed, in source order, by:
//!
//! 1. the produce list — object vnums terminated by `-1` (v3.0 format);
//! 2. `profit_buy`;
//! 3. `profit_sell`;
//! 4. the trade-type list — `ITEM_*` type indices or names, one per line,
//!    terminated by `-1`, each optionally followed by `; buy word`;
//! 5. the seven shop messages (`fread_string` each);
//! 6. broke temper, bitvector, keeper mob vnum, trade-with bitvector;
//! 7. the room list — room vnums terminated by `-1`;
//! 8. open1, close1, open2, close2.
//!
//! Files without the `v3.0` marker use the pre-v3.0 fixed-length lists
//! (`MAX_PROD`/`MAX_TRADE`, no `-1` terminators); the parser honours both. The
//! keeper mob vnum (`SHOP_KEEPER`) is what Circle's `shop_keeper` matches the
//! NPC against, so it — not the record's `#` label — is the shop's key.

use crate::error::Result;
use crate::source::tokens::Source;

/// Pre-v3.0 produce-list length (`MAX_PROD` in `shop.h`).
const MAX_PROD: usize = 5;
/// Pre-v3.0 trade-type-list length (`MAX_TRADE` in `shop.h`).
const MAX_TRADE: usize = 5;
/// Marker that switches the reader to the variable-length v3.0 format.
const VERSION3_TAG: &str = "v3.0";

/// `item_types[]` from `constants.c:382`, in index order.
const ITEM_TYPE_NAMES: [&str; 24] = [
    "UNDEFINED",
    "LIGHT",
    "SCROLL",
    "WAND",
    "STAFF",
    "WEAPON",
    "FURNITURE",
    "FREE",
    "TREASURE",
    "ARMOR",
    "POTION",
    "WORN",
    "OTHER",
    "TRASH",
    "FREE2",
    "CONTAINER",
    "NOTE",
    "LIQ CONTAINER",
    "KEY",
    "FOOD",
    "MONEY",
    "PEN",
    "BOAT",
    "FOUNTAIN",
];

/// One entry of the shop's trade-type list: the `ITEM_*` index and Circle's
/// optional `BUY_WORD` keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopBuyType {
    pub kind: i32,
    pub word: String,
}

/// One `.shp` shop record.
#[derive(Debug, Clone, PartialEq)]
pub struct RawShop {
    /// The record's `#<vnum>` label (`SHOP_NUM`), a shop id, not an NPC id.
    pub record: i32,
    /// The keeper mob vnum (`SHOP_KEEPER`) — the NPC the shop belongs to.
    pub keeper: i32,
    pub produce: Vec<i32>,
    pub profit_buy: f32,
    pub profit_sell: f32,
    pub buy_types: Vec<ShopBuyType>,
    /// Circle's seven messages in source order.
    pub messages: Vec<String>,
    pub broke_temper: i32,
    pub bitvector: i64,
    pub trade_with: i32,
    pub rooms: Vec<i32>,
    pub open1: i32,
    pub close1: i32,
    pub open2: i32,
    pub close2: i32,
}

impl RawShop {
    /// Whether the trade-type list had any `;`-style buy words.
    pub fn buy_words(&self) -> impl Iterator<Item = &str> {
        self.buy_types
            .iter()
            .map(|entry| entry.word.as_str())
            .filter(|word| !word.is_empty())
    }
}

/// Parse every shop in a `.shp` file.
pub fn parse_shops(src: &mut Source) -> Result<Vec<RawShop>> {
    let mut shops = Vec::new();
    let mut new_format = false;
    loop {
        let header = src.read_string("shop header")?;
        let trimmed = header.trim();
        if let Some(rest) = trimmed.strip_prefix('#') {
            let record = rest.trim().parse::<i32>().map_err(|_| {
                src.parse_error(format!("expected `#<shop vnum>`, got `{trimmed}`"))
            })?;
            shops.push(parse_shop(src, record, new_format)?);
        } else if trimmed == "$" {
            break;
        } else if trimmed.contains(VERSION3_TAG) {
            new_format = true;
        }
    }
    Ok(shops)
}

fn parse_shop(src: &mut Source, record: i32, new_format: bool) -> Result<RawShop> {
    let produce = read_int_list(src, new_format, MAX_PROD, "shop produce vnum")?;
    let profit_buy = read_float(src, "shop profit_buy")?;
    let profit_sell = read_float(src, "shop profit_sell")?;
    let buy_types = if new_format {
        read_type_list(src)?
    } else {
        read_legacy_type_list(src)?
    };

    let mut messages = Vec::with_capacity(7);
    for index in 0..7 {
        messages.push(src.read_string(&format!("shop {record} message {index}"))?);
    }

    let broke_temper = read_int(src, "shop broke_temper")?;
    let bitvector = read_int(src, "shop bitvector")? as i64;
    let keeper = read_int(src, "shop keeper")?;
    let trade_with = read_int(src, "shop trade_with")?;
    let rooms = read_int_list(src, new_format, 1, "shop room vnum")?;
    let open1 = read_int(src, "shop open1")?;
    let close1 = read_int(src, "shop close1")?;
    let open2 = read_int(src, "shop open2")?;
    let close2 = read_int(src, "shop close2")?;

    Ok(RawShop {
        record,
        keeper,
        produce,
        profit_buy,
        profit_sell,
        buy_types,
        messages,
        broke_temper,
        bitvector,
        trade_with,
        rooms,
        open1,
        close1,
        open2,
        close2,
    })
}

/// `read_list`: v3.0 reads until the `-1` sentinel; legacy reads `max` entries.
fn read_int_list(src: &mut Source, new_format: bool, max: usize, what: &str) -> Result<Vec<i32>> {
    let mut out = Vec::new();
    if new_format {
        loop {
            let value = read_int(src, what)?;
            if value < 0 {
                break;
            }
            out.push(value);
        }
    } else {
        for _ in 0..max {
            out.push(read_int(src, what)?);
        }
    }
    Ok(out)
}

/// `read_type_list` in v3.0 form: one type per line until `-1`.
fn read_type_list(src: &mut Source) -> Result<Vec<ShopBuyType>> {
    let mut out = Vec::new();
    loop {
        let line = src
            .next_nonblank()
            .ok_or_else(|| src.parse_error("unexpected end of file in shop type list"))?;
        // The source cuts the line at `;`; whatever follows is the buy word.
        let (head, explicit_word) = match line.split_once(';') {
            Some((head, word)) => (head.trim(), word.trim()),
            None => (line.trim(), ""),
        };
        if head.is_empty() {
            continue;
        }
        if head.starts_with("-1") {
            break;
        }
        let (kind, rest) = parse_type_head(src, head)?;
        let word = if explicit_word.is_empty() {
            rest
        } else {
            explicit_word.to_owned()
        };
        out.push(ShopBuyType { kind, word });
    }
    Ok(out)
}

/// The pre-v3.0 type list: `MAX_TRADE` bare integers.
fn read_legacy_type_list(src: &mut Source) -> Result<Vec<ShopBuyType>> {
    let mut out = Vec::with_capacity(MAX_TRADE);
    for _ in 0..MAX_TRADE {
        out.push(ShopBuyType {
            kind: read_int(src, "shop buy type")?,
            word: String::new(),
        });
    }
    Ok(out)
}

/// Resolve one type-list line to an `ITEM_*` index plus any trailing word.
///
/// A named type matches the longest `item_types[]` prefix (so `FREE2` wins
/// over `FREE`); otherwise the leading integer is the index, exactly as the
/// source's `sscanf` fallback does.
fn parse_type_head(src: &Source, head: &str) -> Result<(i32, String)> {
    let mut best: Option<(usize, i32)> = None;
    for (index, name) in ITEM_TYPE_NAMES.iter().enumerate() {
        if head.len() >= name.len() && head[..name.len()].eq_ignore_ascii_case(name) {
            let longer = best.is_none_or(|(best_len, _)| name.len() > best_len);
            if longer {
                best = Some((name.len(), index as i32));
            }
        }
    }
    if let Some((len, kind)) = best {
        return Ok((kind, head[len..].trim_start().to_owned()));
    }

    let digits: String = head
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    let kind = digits
        .parse::<i32>()
        .map_err(|_| src.parse_error(format!("expected an item type, got `{head}`")))?;
    Ok((kind, head[digits.len()..].trim_start().to_owned()))
}

fn read_int(src: &mut Source, what: &str) -> Result<i32> {
    let line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("missing {what}")))?;
    line.split_whitespace()
        .next()
        .and_then(|token| token.parse::<i32>().ok())
        .ok_or_else(|| src.parse_error(format!("expected an integer for {what}, got `{line}`")))
}

fn read_float(src: &mut Source, what: &str) -> Result<f32> {
    let line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("missing {what}")))?;
    line.split_whitespace()
        .next()
        .and_then(|token| token.parse::<f32>().ok())
        .ok_or_else(|| src.parse_error(format!("expected a number for {what}, got `{line}`")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::source::tokens::Source;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn source(text: &str) -> Source {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("tbaport-shop-{}-{serial}.shp", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("create temp shp");
        file.write_all(text.as_bytes()).expect("write temp shp");
        Source::from_path(&path).expect("open temp shp")
    }

    #[test]
    fn parses_a_v3_record_with_all_fields() {
        let mut src = source(
            "CircleMUD v3.0 Shop File~\n#3000~\n82\n-1\n1.20\n0.90\n2\n9\n-1\n\
             %s one~\n%s two~\n%s three~\n%s four~\n%s five~\n%s six~\n%s seven~\n\
             0\n6\n98\n0\n100\n-1\n0\n28\n0\n0\n$~\n",
        );
        let shops = parse_shops(&mut src).expect("parses");
        assert_eq!(shops.len(), 1);
        let shop = &shops[0];
        assert_eq!(shop.record, 3000);
        assert_eq!(shop.keeper, 98);
        assert_eq!(shop.produce, vec![82]);
        assert_eq!(shop.profit_buy, 1.2);
        assert_eq!(shop.profit_sell, 0.9);
        assert_eq!(
            shop.buy_types,
            vec![
                ShopBuyType {
                    kind: 2,
                    word: String::new()
                },
                ShopBuyType {
                    kind: 9,
                    word: String::new()
                },
            ]
        );
        assert_eq!(shop.messages.len(), 7);
        assert_eq!(shop.messages[0], "%s one");
        assert_eq!(shop.bitvector, 6);
        assert_eq!(shop.rooms, vec![100]);
        assert_eq!(
            (shop.open1, shop.close1, shop.open2, shop.close2),
            (0, 28, 0, 0)
        );
    }

    #[test]
    fn parses_named_types_and_buy_words() {
        let mut src = source(
            "CircleMUD v3.0 Shop File~\n#7~\n-1\n2.00\n0.50\nWEAPON; steel\nARMOR\n-1\n\
             ~\n~\n~\n~\n~\n~\n~\n2\n6\n3007\n1\n3050\n-1\n0\n28\n0\n0\n$~\n",
        );
        let shops = parse_shops(&mut src).expect("parses");
        let shop = &shops[0];
        assert_eq!(shop.keeper, 3007);
        assert!(shop.produce.is_empty());
        assert_eq!(shop.buy_types[0].kind, 5, "WEAPON");
        assert_eq!(shop.buy_types[0].word, "steel");
        assert_eq!(shop.buy_types[1].kind, 9, "ARMOR");
    }

    #[test]
    fn an_empty_shop_file_parses_to_no_records() {
        let mut src = source("$~\n");
        assert!(parse_shops(&mut src).expect("parses").is_empty());
    }
}
