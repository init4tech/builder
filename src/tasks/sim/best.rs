use alloy::primitives::U256;

/// An evalutated EVM transaction with a particular score.
#[derive(Debug, Clone)]
pub struct Best<In, Out, S = U256> {
    /// The transaction or bundle being executed.
    input: In,
    /// The result of the tx/bundle execution.
    output: Out,
    /// The score calculated by the evaluation function.
    score: S,
}

impl<In, Out, S> Best<In, Out, S> {
    /// Creates a new `Best` instance.
    pub fn new(input: In, output: Out, evaluator: impl FnOnce(&Out) -> S) -> Self {
        let score = evaluator(&output);
        Self::new_unchecked(input, output, score)
    }

    /// Creates a new `Best` instance without evaluating the score.
    pub fn new_unchecked(input: In, output: Out, score: S) -> Self {
        Self { input, output, score }
    }

    /// Get a reference to the input, usually a transaction or bundle.
    pub fn input(&self) -> &In {
        &self.input
    }

    /// Get a reference to the output.
    pub fn output(&self) -> &Out {
        &self.output
    }

    /// Get a reference to the score.
    pub fn score(&self) -> &S {
        &self.score
    }
}
