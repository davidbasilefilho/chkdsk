//! Only interactive display options may cross the elevation boundary.
#[derive(Debug)]
pub struct Consent {
    arguments: Option<String>,
}
impl Consent {
    pub fn new(args: &[String], resumed: bool) -> Self {
        let mut allowed = Vec::new();
        if !resumed && args.len().is_multiple_of(2) {
            for pair in args.as_chunks::<2>().0 {
                let (flag, value) = (pair[0].as_str(), pair[1].as_str());
                let dimension = |s: &str| {
                    s.len() <= 5
                        && s.bytes().all(|b| b.is_ascii_digit())
                        && s.parse::<u16>().is_ok_and(|n| (1..=16384).contains(&n))
                };
                let valid = match flag {
                    "--theme" => matches!(value, "dark" | "light" | "system"),
                    "--size" => value
                        .split_once('x')
                        .is_some_and(|(w, h)| dimension(w) && dimension(h)),
                    "--scale" => {
                        value.len() <= 8
                            && value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                            && value
                                .parse::<f32>()
                                .is_ok_and(|n| (0.25..=8.0).contains(&n))
                    }
                    _ => false,
                };
                if !valid {
                    return Self { arguments: None };
                }
                allowed.extend([flag, value]);
            }
            Self {
                arguments: Some(allowed.join(" ")),
            }
        } else {
            Self { arguments: None }
        }
    }
    pub fn request(&mut self, needs_permission: bool) -> Option<String> {
        if needs_permission {
            self.arguments.take()
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_and_partial_reads_never_repeat_a_prompt() {
        let args = ["--size", "800x600", "--theme", "dark"].map(str::to_string);
        let mut gate = Consent::new(&args, false);
        assert_eq!(gate.request(false), None);
        assert_eq!(
            gate.request(true).as_deref(),
            Some("--size 800x600 --theme dark")
        );
        assert_eq!(gate.request(true), None);
        assert_eq!(Consent::new(&args, true).request(true), None);
        for args in [
            vec!["--snap", "f.png"],
            vec!["--fixture"],
            vec!["--version"],
            vec!["--scene", "macrovip"],
            vec!["--scale", "2 extra"],
        ] {
            assert_eq!(
                Consent::new(
                    &args.into_iter().map(str::to_string).collect::<Vec<_>>(),
                    false
                )
                .request(true),
                None
            );
        }
    }
}
