//! Distance functions with SIMD acceleration
//!
//! Ported from src/vector/distance.ts

/// Dot product of two vectors
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Squared Euclidean distance
#[inline]
pub fn squared_euclidean(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum()
}

/// Euclidean distance
#[inline]
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    squared_euclidean(a, b).sqrt()
}

/// Cosine similarity (assumes normalized vectors for efficiency)
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    dot_product(a, b)
}

/// Cosine distance (1 - cosine_similarity)
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cosine_similarity(a, b)
}

/// L2 norm of a vector
#[inline]
pub fn l2_norm(v: &[f32]) -> f32 {
    dot_product(v, v).sqrt()
}

/// Normalize a vector in-place
pub fn normalize_in_place(v: &mut [f32]) {
    let norm = l2_norm(v);
    if norm > 1e-10 {
        let inv_norm = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv_norm;
        }
    }
}

/// Normalize a vector, returning a new vector
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let mut result = v.to_vec();
    normalize_in_place(&mut result);
    result
}

/// Check if a vector is normalized (within tolerance)
pub fn is_normalized(v: &[f32], tolerance: f32) -> bool {
    let norm = l2_norm(v);
    (norm - 1.0).abs() < tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dot_product() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        assert_eq!(dot_product(&a, &b), 32.0);
    }

    #[test]
    fn test_squared_euclidean() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        assert_eq!(squared_euclidean(&a, &b), 2.0);
    }

    #[test]
    fn test_normalize() {
        let v = [3.0, 4.0];
        let n = normalize(&v);
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[1] - 0.8).abs() < 1e-6);
        assert!(is_normalized(&n, 1e-6));
    }
}
