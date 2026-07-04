use crate::api::C2cStreamState;

#[derive(Debug)]
pub(crate) enum C2cStreamingPhase {
    Pending(C2cStreamState),
    Active(C2cStreamState),
    BrokenActive(C2cStreamState),
    Completed,
}

impl C2cStreamingPhase {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Pending(_) => "pending",
            Self::Active(_) => "active",
            Self::BrokenActive(_) => "broken_active",
            Self::Completed => "completed",
        }
    }
}
