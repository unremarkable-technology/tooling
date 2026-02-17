//! Edit distance calculation for "Did you mean..." suggestions

/// Calculate Levenshtein distance between two strings
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
	let a_len = a.chars().count();
	let b_len = b.chars().count();

	if a_len == 0 {
		return b_len;
	}
	if b_len == 0 {
		return a_len;
	}

	let a_chars: Vec<char> = a.chars().collect();
	let b_chars: Vec<char> = b.chars().collect();

	let mut prev_row: Vec<usize> = (0..=b_len).collect();
	let mut curr_row: Vec<usize> = vec![0; b_len + 1];

	for i in 1..=a_len {
		curr_row[0] = i;

		for j in 1..=b_len {
			let cost = if a_chars[i - 1] == b_chars[j - 1] {
				0
			} else {
				1
			};

			curr_row[j] = *[
				prev_row[j] + 1,        // deletion
				curr_row[j - 1] + 1,    // insertion
				prev_row[j - 1] + cost, // substitution
			]
			.iter()
			.min()
			.unwrap();
		}

		std::mem::swap(&mut prev_row, &mut curr_row);
	}

	prev_row[b_len]
}

/// Find the best suggestion from a list of candidates
/// Returns (suggestion, distance) if a good match is found
pub fn find_closest<'a>(
	typo: &str,
	candidates: impl Iterator<Item = &'a str>,
) -> Option<(&'a str, usize)> {
	let typo_lower = typo.to_lowercase();

	let mut best: Option<(&str, usize)> = None;

	for candidate in candidates {
		let candidate_lower = candidate.to_lowercase();
		let distance = levenshtein_distance(&typo_lower, &candidate_lower);

		if let Some((_, best_distance)) = best {
			if distance < best_distance {
				best = Some((candidate, distance));
			}
		} else {
			best = Some((candidate, distance));
		}
	}

	best
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_levenshtein_distance() {
		assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
		assert_eq!(levenshtein_distance("saturday", "sunday"), 3);
		assert_eq!(levenshtein_distance("", "hello"), 5);
		assert_eq!(levenshtein_distance("hello", ""), 5);
		assert_eq!(levenshtein_distance("same", "same"), 0);
	}

	#[test]
	fn test_find_suggestion_exact_match() {
		let candidates = ["DomainName", "Arn", "WebsiteURL"];
		let result = find_closest("DomainName", candidates.iter().copied());

		assert_eq!(result, Some(("DomainName", 0)));
	}

	#[test]
	fn test_find_suggestion_close_match() {
		let candidates = ["DomainName", "Arn", "WebsiteURL"];
		let result = find_closest("DomainName2", candidates.iter().copied());

		assert_eq!(result, Some(("DomainName", 1)));
	}

	#[test]
	fn test_find_suggestion_typo() {
		let candidates = ["BucketName", "Arn", "WebsiteURL"];
		let result = find_closest("BucketNam", candidates.iter().copied());

		assert_eq!(result, Some(("BucketName", 1)));
	}

	#[test]
	fn test_find_suggestion_no_match() {
		let candidates = ["DomainName", "Arn", "WebsiteURL"];
		let result = find_closest("CompletelyDifferent", candidates.iter().copied());

		assert_eq!(result, Some(("DomainName", 15)));
	}

	#[test]
	fn test_find_suggestion_case_insensitive() {
		let candidates = ["DomainName", "Arn", "WebsiteURL"];
		let result = find_closest("domainname2", candidates.iter().copied());

		assert_eq!(result, Some(("DomainName", 1)));
	}
}
