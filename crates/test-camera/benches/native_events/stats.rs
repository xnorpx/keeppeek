#[derive(Debug)]
pub struct Summary {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
}

pub fn summarize(samples: &[f64], expected_count: usize) -> Result<Summary, &'static str> {
    if !(10..=20).contains(&expected_count) || samples.len() != expected_count {
        return Err("expected the exact declared count of 10 to 20 samples");
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err("every measurement must be finite");
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    Ok(Summary {
        count: ordered.len(),
        p50: ordered[nearest_rank(ordered.len(), 50)?],
        p95: ordered[nearest_rank(ordered.len(), 95)?],
        min: ordered[0],
        max: ordered[ordered.len() - 1],
    })
}

fn nearest_rank(count: usize, percentile: usize) -> Result<usize, &'static str> {
    if count == 0 || !(1..=100).contains(&percentile) {
        return Err("nearest rank requires samples and a percentile from 1 to 100");
    }
    count
        .checked_mul(percentile)
        .ok_or("nearest rank multiplication overflow")?
        .div_ceil(100)
        .checked_sub(1)
        .filter(|index| *index < count)
        .ok_or("nearest rank index out of bounds")
}

#[cfg(test)]
mod tests {
    #[test]
    fn nearest_rank_checks_bounds_and_overflow() {
        use super::nearest_rank;

        assert_eq!(nearest_rank(10, 50).expect("median index"), 4);
        assert_eq!(nearest_rank(10, 95).expect("tail index"), 9);
        assert_eq!(nearest_rank(20, 95).expect("nineteenth index"), 18);
        assert_eq!(nearest_rank(1, 100).expect("single sample"), 0);
        nearest_rank(0, 50).expect_err("empty samples");
        nearest_rank(10, 0).expect_err("zero percentile");
        nearest_rank(10, 101).expect_err("percentile above 100");
        nearest_rank(usize::MAX, 95).expect_err("rank multiplication overflow");
    }

    #[test]
    fn ten_samples_use_nearest_rank_without_reordering_input() {
        use super::summarize;

        let samples = [10.0, 1.0, 9.0, 2.0, 8.0, 3.0, 7.0, 4.0, 6.0, 5.0];
        let summary = summarize(&samples, 10).expect("complete finite sample set");
        assert_eq!(summary.count, 10);
        assert!((summary.p50 - 5.0).abs() < f64::EPSILON);
        assert!((summary.p95 - 10.0).abs() < f64::EPSILON);
        assert!((summary.min - 1.0).abs() < f64::EPSILON);
        assert!((summary.max - 10.0).abs() < f64::EPSILON);
        assert!((samples[0] - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn twenty_samples_do_not_round_p95_up_to_the_maximum() {
        use super::summarize;

        let samples: Vec<_> = (1..=20).map(f64::from).collect();
        let summary = summarize(&samples, 20).expect("twenty samples");
        assert!((summary.p50 - 10.0).abs() < f64::EPSILON);
        assert!((summary.p95 - 19.0).abs() < f64::EPSILON);
        assert!((summary.max - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_extra_or_nonfinite_samples_are_not_silently_dropped() {
        use super::summarize;

        summarize(&[1.0; 9], 10).expect_err("missing sample");
        summarize(&[1.0; 11], 10).expect_err("extra sample");
        summarize(&[], 0).expect_err("empty workload");
        summarize(&[1.0; 21], 21).expect_err("sample bound");
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut samples = [1.0; 10];
            samples[4] = invalid;
            summarize(&samples, 10).expect_err("nonfinite sample");
        }
    }

    #[test]
    fn paired_deltas_preserve_negative_values() {
        use super::summarize;

        let samples: Vec<_> = (-5..5).map(f64::from).collect();
        let summary = summarize(&samples, 10).expect("signed paired differences");
        assert!((summary.p50 + 1.0).abs() < f64::EPSILON);
        assert!((summary.p95 - 4.0).abs() < f64::EPSILON);
    }
}
