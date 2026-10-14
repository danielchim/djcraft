//! Musical keys on the Camelot wheel.
//!
//! The wheel arranges the 24 keys so that keys which mix well sit next to
//! each other: numbers 1 to 12 around the circle, with `A` for minor keys on
//! the inside and `B` for major keys on the outside.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Camelot {
    /// 1 to 12.
    pub number: u8,
    /// True for the `B` ring.
    pub major: bool,
}

impl Camelot {
    pub fn new(number: u8, major: bool) -> Option<Self> {
        (1..=12).contains(&number).then_some(Self { number, major })
    }

    /// The key whose home note is `tonic` semitones above C (0 is C, 9 is
    /// A), in its major or minor form.
    pub fn from_tonic(tonic: u8, major: bool) -> Self {
        // A minor key shares its number with the major key three semitones up.
        let relative = (tonic as u32 + if major { 0 } else { 3 }) % 12;
        // Each step round the wheel is a fifth (seven semitones); C major is 8B.
        Self {
            number: ((relative * 7 + 7) % 12 + 1) as u8,
            major,
        }
    }

    /// All 24 keys, by number and then minor before major.
    pub fn all() -> impl Iterator<Item = Camelot> {
        (1..=12).flat_map(|number| [false, true].map(|major| Camelot { number, major }))
    }

    /// Reads a key however a tag might spell it: Camelot (`8A`, `08a`),
    /// Open Key (`1m`, `1d`), or a note name (`Am`, `A minor`, `F#`, `Bbmaj`).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            let number: u8 = text[..digits].parse().ok()?;
            let suffix = text[digits..].trim().to_ascii_lowercase();
            return match suffix.as_str() {
                "a" => Self::new(number, false),
                "b" => Self::new(number, true),
                // Open Key counts from C major and A minor.
                "m" | "d" if (1..=12).contains(&number) => {
                    Self::new((number + 6) % 12 + 1, suffix == "d")
                }
                _ => None,
            };
        }

        let mut chars = text.chars();
        let mut pitch: i32 = match chars.next()?.to_ascii_uppercase() {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => return None,
        };
        let mut rest = chars.as_str();
        if let Some(after) = rest.strip_prefix(['#', '♯']) {
            pitch += 1;
            rest = after;
        } else if let Some(after) = rest.strip_prefix(['b', '♭']) {
            pitch -= 1;
            rest = after;
        }
        let major = match rest.trim().to_ascii_lowercase().as_str() {
            "" | "maj" | "major" | "dur" => true,
            "m" | "min" | "minor" | "moll" => false,
            _ => return None,
        };
        Some(Self::from_tonic(pitch.rem_euclid(12) as u8, major))
    }

    /// The keys that mix smoothly with this one: itself, one step either way
    /// round the wheel, and the relative major or minor.
    pub fn compatible(self) -> [Camelot; 4] {
        let step = |by: u8| Camelot {
            number: (self.number - 1 + by) % 12 + 1,
            major: self.major,
        };
        [
            self,
            step(1),
            step(11),
            Camelot {
                number: self.number,
                major: !self.major,
            },
        ]
    }

    /// The key a number of semitones above (or, if negative, below) this
    /// one. Each semitone is seven steps round the wheel.
    pub fn shifted(self, semitones: i32) -> Camelot {
        let steps = (self.number as i32 - 1 + 7 * semitones).rem_euclid(12);
        Camelot {
            number: steps as u8 + 1,
            major: self.major,
        }
    }

    /// The key's usual name, such as `A minor` or `F♯ major`.
    pub fn name(self) -> String {
        const MAJORS: [&str; 12] = [
            "B", "F♯", "D♭", "A♭", "E♭", "B♭", "F", "C", "G", "D", "A", "E",
        ];
        const MINORS: [&str; 12] = [
            "A♭", "E♭", "B♭", "F", "C", "G", "D", "A", "E", "B", "F♯", "D♭",
        ];
        let index = self.number as usize - 1;
        if self.major {
            format!("{} major", MAJORS[index])
        } else {
            format!("{} minor", MINORS[index])
        }
    }
}

impl fmt::Display for Camelot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.number, if self.major { 'B' } else { 'A' })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Option<String> {
        Camelot::parse(text).map(|key| key.to_string())
    }

    #[test]
    fn reads_keys_in_every_spelling() {
        for (text, expected) in [
            ("8A", "8A"),
            ("08a", "8A"),
            ("12B", "12B"),
            ("Am", "8A"),
            ("A minor", "8A"),
            ("Amin", "8A"),
            ("C", "8B"),
            ("C major", "8B"),
            ("F#m", "11A"),
            ("F♯ minor", "11A"),
            ("Gbm", "11A"),
            ("Bb", "6B"),
            ("Bbm", "3A"),
            ("B", "1B"),
            ("Db", "3B"),
            ("C#m", "12A"),
            ("E", "12B"),
            ("G#m", "1A"),
            (" Dm ", "7A"),
            // Open Key
            ("1d", "8B"),
            ("1m", "8A"),
            ("6m", "1A"),
            ("12d", "7B"),
        ] {
            assert_eq!(code(text).as_deref(), Some(expected), "{text}");
        }
        for text in ["", "13A", "0B", "H", "Am7", "8C", "minor", "13m"] {
            assert_eq!(code(text), None, "{text}");
        }
    }

    #[test]
    fn names_agree_with_parsing() {
        for key in Camelot::all() {
            assert_eq!(Camelot::parse(&key.name()), Some(key), "{key}");
            assert_eq!(Camelot::parse(&key.to_string()), Some(key));
        }
        assert_eq!(Camelot::from_tonic(9, false), Camelot::parse("Am").unwrap());
        assert_eq!(Camelot::from_tonic(0, true), Camelot::parse("C").unwrap());
        assert_eq!(Camelot::from_tonic(6, true).name(), "F♯ major");
        assert_eq!(Camelot::parse("8A").unwrap().name(), "A minor");
        assert_eq!(Camelot::parse("2B").unwrap().name(), "F♯ major");
    }

    #[test]
    fn shifting_moves_by_semitones() {
        let key = |text: &str| Camelot::parse(text).unwrap();
        // A minor up a semitone is B♭ minor; down one is A♭ minor.
        assert_eq!(key("Am").shifted(1), key("Bbm"));
        assert_eq!(key("Am").shifted(-1), key("G#m"));
        assert_eq!(key("C").shifted(2), key("D"));
        assert_eq!(key("C").shifted(12), key("C"));
        assert_eq!(key("C").shifted(-14), key("Bb"));
        for key in Camelot::all() {
            assert_eq!(key.shifted(5).shifted(-5), key);
        }
    }

    #[test]
    fn compatible_keys_are_neighbours_and_the_relative() {
        let codes = |text: &str| {
            Camelot::parse(text)
                .unwrap()
                .compatible()
                .map(|key| key.to_string())
        };
        assert_eq!(codes("8A"), ["8A", "9A", "7A", "8B"]);
        assert_eq!(codes("12B"), ["12B", "1B", "11B", "12A"]);
        assert_eq!(codes("1A"), ["1A", "2A", "12A", "1B"]);
    }
}
