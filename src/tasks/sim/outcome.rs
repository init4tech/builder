use alloy::primitives::U256;
use trevm::revm::database::Cache;

/// An evaluated EVM transaction with a particular score.
#[derive(Debug, Clone)]
pub struct SimOutcome<In, Out = Cache, S = U256> {
    /// The transaction or bundle being executed.
    input: In,
    /// The result of the tx/bundle execution.
    output: Out,
    /// The score calculated by the evaluation function.
    score: S,
}

impl<In, Out, S> SimOutcome<In, Out, S> {
    /// Creates a new `Best` instance.
    pub fn new(input: In, output: Out, evaluator: impl FnOnce(&Out) -> S) -> Self {
        let score = evaluator(&output);
        Self::new_unchecked(input, output, score)
    }

    /// Transform the input type to a new type, while keeping the output and
    /// score the same.
    pub fn map_in_into<In2>(self) -> SimOutcome<In2, Out, S>
    where
        In2: From<In>,
    {
        self.map_in(Into::into)
    }

    /// Transform the input type to a new type using a closure.
    pub fn map_in<In2, F>(self, f: F) -> SimOutcome<In2, Out, S>
    where
        In2: From<In>,
        F: FnOnce(In) -> In2,
    {
        let input = f(self.input);
        SimOutcome { input, output: self.output, score: self.score }
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
