// Private aggregate finalization state and transition helpers.

#[derive(Debug, Copy, Clone)]
enum HalfVecAggregateFinal {
    Sum,
    Average,
}

#[derive(Debug, Copy, Clone)]
enum SparseVecAggregateFinal {
    Sum,
    Average,
}

#[derive(Debug, Copy, Clone)]
enum BitVecAggregateOp {
    Or,
    And,
}

fn bitvec_bool_transition(
    state: Option<Vec<bool>>,
    value: Option<BitVec>,
    operation: BitVecAggregateOp,
) -> Option<Vec<bool>> {
    let Some(value) = value else {
        return state;
    };
    let value = bitvec_to_core(value);

    let mut state = match state {
        Some(state) => state,
        None => return Some(value.as_slice().to_vec()),
    };
    if state.len() != value.len() {
        raise_core_error(CoreError::DimensionMismatch {
            left: state.len(),
            right: value.len(),
        });
    }

    for (accumulated, value) in state.iter_mut().zip(value.as_slice()) {
        match operation {
            BitVecAggregateOp::Or => *accumulated |= *value,
            BitVecAggregateOp::And => *accumulated &= *value,
        }
    }
    Some(state)
}

fn halfvec_from_aggregate_state(mut state: Vec<f32>, aggregate: HalfVecAggregateFinal) -> HalfVec {
    if state.len() < 2 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "halfvec aggregate state is missing dimensions",
        );
    }
    let count = state.remove(0);
    if count <= 0.0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "halfvec aggregate state has no rows",
        );
    }
    if matches!(aggregate, HalfVecAggregateFinal::Average) {
        for value in &mut state {
            *value /= count;
        }
    }

    halfvec_from_values(state)
}

fn sparsevec_from_aggregate_state(
    mut state: Vec<f32>,
    aggregate: SparseVecAggregateFinal,
) -> SparseVec {
    if state.len() < 2 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "sparsevec aggregate state is missing dimensions",
        );
    }
    let count = state.remove(0);
    if count <= 0.0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "sparsevec aggregate state has no rows",
        );
    }
    if matches!(aggregate, SparseVecAggregateFinal::Average) {
        for value in &mut state {
            *value /= count;
        }
    }

    let dimensions = state.len();
    let entries = state
        .into_iter()
        .enumerate()
        .filter(|(_, value)| *value != 0.0)
        .map(|(offset, value)| {
            SparseEntry::new(offset + 1, value).unwrap_or_else(|error| raise_core_error(error))
        })
        .collect::<Vec<_>>();
    SparseVec::from_sparse(
        SparseVector::new(dimensions, entries).unwrap_or_else(|error| raise_core_error(error)),
    )
}
