//! Shopping input parsing and exact/plural-aware name resolution.

use crate::client::types::Unit;

/// Case-insensitive exact match with simple plural handling.
pub(crate) fn name_matches(query: &str, name: &str, plural_name: Option<&str>) -> bool {
    let query = query.trim().to_lowercase();
    let same = |candidate: &str| {
        let candidate = candidate.trim().to_lowercase();
        candidate == query
            || candidate == format!("{query}s")
            || candidate == format!("{query}es")
            || query == format!("{candidate}s")
            || query == format!("{candidate}es")
    };
    same(name) || plural_name.is_some_and(same)
}

const UNIT_ALIASES: &[&[&str]] = &[
    &["lb", "lbs", "pound", "pounds"],
    &["oz", "ounce", "ounces"],
    &["g", "gram", "grams"],
    &["kg", "kgs", "kilo", "kilos", "kilogram", "kilograms"],
    &[
        "ml",
        "milliliter",
        "milliliters",
        "millilitre",
        "millilitres",
    ],
    &["l", "liter", "liters", "litre", "litres"],
    &["cup", "cups"],
    &["tbsp", "tbs", "tablespoon", "tablespoons"],
    &["tsp", "teaspoon", "teaspoons"],
    &["can", "cans"],
    &["jar", "jars"],
    &["bag", "bags"],
    &["box", "boxes"],
    &["bunch", "bunches"],
    &["pack", "packs", "package", "packages", "pkg"],
    &["bottle", "bottles"],
    &["head", "heads"],
    &["clove", "cloves"],
    &["loaf", "loaves"],
    &["carton", "cartons"],
    &["stick", "sticks"],
    &["pint", "pints"],
    &["quart", "quarts"],
    &["gallon", "gallons", "gal"],
];

fn unit_group(word: &str) -> Option<&'static [&'static str]> {
    let word = word.trim().trim_end_matches('.').to_lowercase();
    UNIT_ALIASES
        .iter()
        .copied()
        .find(|group| group.contains(&word.as_str()))
}

pub(crate) fn resolve_unit_name(unit: &str, existing: &[Unit]) -> String {
    let unit = unit.trim();
    let group = unit_group(unit);
    let means_same = |name: &str| {
        let name = name.trim().to_lowercase();
        name == unit.to_lowercase() || group.is_some_and(|aliases| aliases.contains(&name.as_str()))
    };
    existing
        .iter()
        .find(|known| {
            means_same(&known.name) || known.plural_name.as_deref().is_some_and(means_same)
        })
        .map(|known| known.name.clone())
        .unwrap_or_else(|| {
            group
                .map(|aliases| aliases[0].to_string())
                .unwrap_or_else(|| unit.to_string())
        })
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedShoppingItem {
    pub food: String,
    pub amount: Option<f64>,
    pub unit: Option<String>,
}

fn parse_quantity(word: &str) -> Option<f64> {
    let word = word.trim().to_lowercase();
    let vulgar = |character: char| match character {
        '½' => Some(0.5),
        '¼' => Some(0.25),
        '¾' => Some(0.75),
        '⅓' => Some(1.0 / 3.0),
        '⅔' => Some(2.0 / 3.0),
        '⅛' => Some(0.125),
        _ => None,
    };
    let number_words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    if word == "a" || word == "an" {
        return Some(1.0);
    }
    if let Some(index) = number_words.iter().position(|candidate| *candidate == word) {
        return Some(index as f64 + 1.0);
    }
    if let Some((numerator, denominator)) = word.split_once('/') {
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        return (denominator != 0.0).then_some(numerator / denominator);
    }
    if let Some(fraction) = word.chars().last().and_then(vulgar) {
        let last = word.chars().last().expect("last character exists");
        let whole = &word[..word.len() - last.len_utf8()];
        return if whole.is_empty() {
            Some(fraction)
        } else {
            whole.parse::<f64>().ok().map(|value| value + fraction)
        };
    }
    word.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Parses natural shopping text without inventing Tandoor identifiers.
pub(crate) fn parse_shopping_text(text: &str, extra_units: &[String]) -> ParsedShoppingItem {
    let text = text.trim().trim_end_matches(['.', ',', ';']).trim();
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let plain = || ParsedShoppingItem {
        food: text.to_string(),
        amount: None,
        unit: None,
    };

    let mut amount = None;
    if words.len() >= 2 {
        let last = words[words.len() - 1].to_lowercase();
        let suffix = last.strip_prefix('x').or_else(|| last.strip_prefix('×'));
        if let Some(value) = suffix
            .filter(|value| !value.is_empty())
            .and_then(parse_quantity)
        {
            amount = Some(value);
            words.pop();
        } else if words.len() >= 3
            && (words[words.len() - 2] == "x" || words[words.len() - 2] == "×")
        {
            if let Some(value) = parse_quantity(&last) {
                amount = Some(value);
                words.truncate(words.len() - 2);
            }
        }
    }

    let mut index = 0;
    if amount.is_none() {
        if let Some(value) = words.first().and_then(|word| parse_quantity(word)) {
            amount = Some(value);
            index = 1;
            if let Some(fraction) = words
                .get(1)
                .filter(|word| word.contains('/') || word.chars().any(|c| "½¼¾⅓⅔⅛".contains(c)))
                .and_then(|word| parse_quantity(word))
            {
                if fraction < 1.0 {
                    amount = Some(value + fraction);
                    index = 2;
                }
            }
        }
    }

    let mut unit = None;
    if amount.is_some() && words.len() > index + 1 {
        let word = words[index].to_lowercase();
        if word == "dozen" {
            amount = amount.map(|value| value * 12.0);
            index += 1;
        } else if unit_group(&word).is_some()
            || extra_units
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&word))
        {
            unit = Some(words[index].trim_end_matches('.').to_string());
            index += 1;
        }
        if words.len() > index + 1 && words[index].eq_ignore_ascii_case("of") {
            index += 1;
        }
    }

    let food = words[index..].join(" ");
    if food.is_empty() {
        return plain();
    }
    ParsedShoppingItem { food, amount, unit }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_exactly_or_as_simple_plurals() {
        assert!(name_matches("tomatoes", "Tomato", None));
        assert!(name_matches("egg", "Egg", Some("Eggs")));
        assert!(!name_matches("salt", "Salted Butter", None));
    }

    #[test]
    fn shopping_text_parses_amounts_and_units() {
        let parsed = parse_shopping_text("1 1/2 cups flour", &[]);
        assert_eq!(parsed.amount, Some(1.5));
        assert_eq!(parsed.unit.as_deref(), Some("cups"));
        assert_eq!(parsed.food, "flour");
    }
}
