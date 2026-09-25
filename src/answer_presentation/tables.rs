//! Data table (#45), comparison (#46) and KPI cards (#47).
use super::period::scalar_text;
use super::resolve::{ensure, fail, numeric, valid_relative, Checked, Issue, QualifierKind, Rows, Scope};
use crate::answer_blocks::{
    BasisIssue, BlockIssue, ChangeBasis, Collection, ComparisonBlock, ComparisonRow, Coverage, DataTableBlock, Derivation,
    DerivationOp, Fact, Granularity, KpiBlock, KpiCard, KpiChange, PeriodQualifier, Qualifier, QualifierBasis, RowBasis,
    TableColumn, TableRow, ValueRef, MAX_COLUMNS, MAX_ENTITIES, MAX_EXPLICIT_ROWS, MAX_KPI_CARDS,
};
use std::collections::BTreeSet;

/// A qualifier written against each record of a collection, not a whole document.
pub(super) fn relative(qualifier: &Qualifier) -> bool {
    qualifier.result_ref.is_none() && qualifier.derivation_id.is_none()
}

fn relative_period(period: &PeriodQualifier) -> bool {
    period.result_ref.is_none() && period.derivation_id.is_none()
}

/// Qualifier value only: the origin is implied by the collection.
fn implied(value: String) -> Qualifier {
    Qualifier { value: Some(value), basis: Some(QualifierBasis::Evidence), ..Qualifier::default() }
}

pub(super) fn data_table(block: DataTableBlock, scope: &mut Scope) -> Checked<(DataTableBlock, Option<Coverage>)> {
    ensure(!block.columns.is_empty(), BlockIssue::Schema, "columns")?;
    ensure(block.columns.len() <= MAX_COLUMNS, BlockIssue::TooManyItems, "columns")?;
    for (index, column) in block.columns.iter().enumerate() {
        scope.item_id(&column.id, &format!("columns/{index}"))?;
        scope.label(&column.label, &format!("columns/{index}/label"))?;
    }
    if let Some(sort) = &block.sort {
        ensure(block.columns.iter().any(|column| column.id == sort.column_id), BlockIssue::UnknownReference, "sort/column_id")?;
    }
    match block.collection.clone() {
        Some(collection) => collection_table(block, &collection, scope),
        None => Ok((explicit_table(block, scope)?, None)),
    }
}

/// A collection qualifier: one value the selected records share, or one per record.
enum PerRecord<T> {
    Shared(Option<T>),
    Each(Vec<Option<T>>),
}

impl<T: Clone + PartialEq> PerRecord<T> {
    fn from_values(values: Vec<Option<T>>) -> Self {
        if values.windows(2).all(|pair| pair[0] == pair[1]) {
            Self::Shared(values.into_iter().next().flatten())
        } else {
            Self::Each(values)
        }
    }
    fn own(&self, index: usize) -> Option<T> {
        match self {
            Self::Shared(_) => None,
            Self::Each(values) => values[index].clone(),
        }
    }
    fn shared(self) -> Option<T> {
        match self {
            Self::Shared(value) => value,
            Self::Each(_) => None,
        }
    }
}

fn column_qualifier(scope: &mut Scope, rows: &Rows, qualifier: Option<&Qualifier>, kind: QualifierKind, result_ref: &str, path: &str) -> Checked<PerRecord<Qualifier>> {
    match qualifier {
        Some(q) if relative(q) => {
            let values = rows.qualifiers(kind, q.pointer.as_deref().unwrap_or_default(), path)?;
            Ok(match PerRecord::from_values(values) {
                PerRecord::Shared(value) => PerRecord::Shared(value.map(|value| Qualifier { pointer: q.pointer.clone(), ..implied(value) })),
                PerRecord::Each(values) => PerRecord::Each(values.into_iter().map(|value| value.map(implied)).collect()),
            })
        }
        other => Ok(PerRecord::Shared(scope.qualifier(other, kind, Some(result_ref), path)?)),
    }
}

fn column_period(scope: &mut Scope, rows: &Rows, period: Option<&PeriodQualifier>, result_ref: &str, path: &str) -> Checked<PerRecord<PeriodQualifier>> {
    match period {
        Some(p) if relative_period(p) => Ok(match PerRecord::from_values(rows.periods(p, path)?) {
            PerRecord::Shared(value) => PerRecord::Shared(value.map(|shared| PeriodQualifier { pointer: p.pointer.clone(), end_pointer: p.end_pointer.clone(), ..shared })),
            each => each,
        }),
        other => Ok(PerRecord::Shared(scope.period(other, Some(result_ref), path)?)),
    }
}

/// Rows copied from the selected records; the server never truncates them.
/// Records in different units, currencies or periods keep their own per cell.
fn collection_table(block: DataTableBlock, collection: &Collection, scope: &mut Scope) -> Checked<(DataTableBlock, Option<Coverage>)> {
    let rows = scope.collection(collection, "collection")?;
    let mut table: Vec<TableRow> = rows.records.iter().map(|(index, _)| TableRow { row_index: Some(*index), cells: Vec::new() }).collect();
    let mut columns = Vec::with_capacity(block.columns.len());
    for (c, column) in block.columns.iter().enumerate() {
        let path = format!("columns/{c}");
        let pointer = column.pointer.as_deref().filter(|p| valid_relative(p));
        let pointer = pointer.ok_or_else(|| Issue { code: BlockIssue::InvalidPointer, path: format!("{path}/pointer") })?;
        let unit = column_qualifier(scope, &rows, column.unit.as_ref(), QualifierKind::Unit, &collection.result_ref, &format!("{path}/unit"))?;
        let currency = column_qualifier(scope, &rows, column.currency.as_ref(), QualifierKind::Currency, &collection.result_ref, &format!("{path}/currency"))?;
        let period = column_period(scope, &rows, column.period.as_ref(), &collection.result_ref, &format!("{path}/period"))?;
        for (r, (_, record)) in rows.records.iter().enumerate() {
            let cell = Rows::cell(record, pointer, &format!("rows/{r}/cells/{c}"))?.map(|cell| Fact {
                unit: unit.own(r),
                currency: currency.own(r),
                period: period.own(r),
                ..cell
            });
            table[r].cells.push(cell);
        }
        columns.push(TableColumn { unit: unit.shared(), currency: currency.shared(), period: period.shared(), ..column.clone() });
    }
    let coverage = rows.coverage;
    Ok((DataTableBlock { columns, rows: table, ..block }, Some(coverage)))
}

/// Model-written rows of evidence facts, assembled across results.
fn explicit_table(block: DataTableBlock, scope: &mut Scope) -> Checked<DataTableBlock> {
    ensure(block.rows.len() <= MAX_EXPLICIT_ROWS, BlockIssue::TooManyItems, "rows")?;
    let mut columns = Vec::with_capacity(block.columns.len());
    for (index, column) in block.columns.iter().enumerate() {
        let path = format!("columns/{index}");
        columns.push(TableColumn {
            pointer: None,
            unit: scope.qualifier(column.unit.as_ref(), QualifierKind::Unit, None, &format!("{path}/unit"))?,
            currency: scope.qualifier(column.currency.as_ref(), QualifierKind::Currency, None, &format!("{path}/currency"))?,
            period: scope.period(column.period.as_ref(), None, &format!("{path}/period"))?,
            ..column.clone()
        });
    }
    let mut rows = Vec::with_capacity(block.rows.len());
    for (r, row) in block.rows.iter().enumerate() {
        ensure(row.cells.len() == columns.len(), BlockIssue::WidthMismatch, format!("rows/{r}/cells"))?;
        let cells = row
            .cells
            .iter()
            .enumerate()
            .map(|(c, cell)| scope.optional_fact(cell.as_ref(), &format!("rows/{r}/cells/{c}")))
            .collect::<Checked<Vec<_>>>()?;
        rows.push(TableRow { row_index: None, cells });
    }
    Ok(DataTableBlock { collection: None, columns, rows, ..block })
}

/// Differences between cells are named, never resolved into a winner.
pub(super) fn row_basis(cells: &[Option<Fact>]) -> RowBasis {
    let known: Vec<&Fact> = cells.iter().flatten().filter(|fact| !fact.value.is_null()).collect();
    let differs = |key: fn(&Fact) -> Option<&str>| known.iter().map(|fact| key(fact)).collect::<BTreeSet<_>>().len() > 1;
    let mut issues = Vec::new();
    if differs(|fact| fact.unit.as_ref().and_then(|q| q.value.as_deref())) {
        issues.push(BasisIssue::Unit);
    }
    if differs(|fact| fact.currency.as_ref().and_then(|q| q.value.as_deref())) {
        issues.push(BasisIssue::Currency);
    }
    if differs(|fact| fact.period.as_ref().and_then(|p| p.key.as_deref())) {
        issues.push(BasisIssue::Period);
    }
    if differs(|fact| fact.field.as_deref()) {
        issues.push(BasisIssue::Definition);
    }
    let unqualified = known.iter().all(|fact| fact.unit.is_none() && fact.currency.is_none() && fact.period.is_none());
    if unqualified && known.iter().any(|fact| numeric(&fact.value).is_some()) {
        issues.push(BasisIssue::QualifiersUnknown);
    }
    let values: BTreeSet<String> = known
        .iter()
        .map(|fact| numeric(&fact.value).map_or_else(|| scalar_text(&fact.value), |number| number.to_string()))
        .collect();
    RowBasis { comparable: issues.is_empty(), issues, values_differ: values.len() > 1 }
}

pub(super) fn comparison(block: ComparisonBlock, scope: &mut Scope) -> Checked<ComparisonBlock> {
    ensure(block.entities.len() >= 2, BlockIssue::Schema, "entities")?;
    ensure(block.entities.len() <= MAX_ENTITIES, BlockIssue::TooManyItems, "entities")?;
    ensure(!block.rows.is_empty(), BlockIssue::Schema, "rows")?;
    ensure(block.rows.len() <= MAX_EXPLICIT_ROWS, BlockIssue::TooManyItems, "rows")?;
    let mut entities = Vec::with_capacity(block.entities.len());
    for (index, entity) in block.entities.into_iter().enumerate() {
        let path = format!("entities/{index}");
        scope.item_id(&entity.id, &path)?;
        scope.label(&entity.label, &format!("{path}/label"))?;
        let name = scope.optional_fact(entity.name.as_ref(), &format!("{path}/name"))?;
        entities.push(crate::answer_blocks::ComparedEntity { name, ..entity });
    }
    let mut rows = Vec::with_capacity(block.rows.len());
    for (r, row) in block.rows.into_iter().enumerate() {
        let path = format!("rows/{r}");
        scope.item_id(&row.id, &path)?;
        scope.label(&row.label, &format!("{path}/label"))?;
        if let Some(requirement) = &row.requirement_id {
            ensure(scope.ctx.requirements.iter().any(|r| r.id == *requirement), BlockIssue::UnknownReference, format!("{path}/requirement_id"))?;
        }
        ensure(row.cells.len() == entities.len(), BlockIssue::WidthMismatch, format!("{path}/cells"))?;
        let cells = row
            .cells
            .iter()
            .enumerate()
            .map(|(c, cell)| scope.optional_fact(cell.as_ref(), &format!("{path}/cells/{c}")))
            .collect::<Checked<Vec<_>>>()?;
        rows.push(ComparisonRow { basis: Some(row_basis(&cells)), cells, ..row });
    }
    Ok(ComparisonBlock { entities, rows })
}

fn same_origin(a: &ValueRef, b: &ValueRef) -> bool {
    a.result_ref == b.result_ref && a.pointer == b.pointer && a.derivation_id == b.derivation_id
}

fn differs_from(derivation: &Derivation, fact: &ValueRef, baseline: &ValueRef) -> bool {
    derivation.op == DerivationOp::Difference
        && derivation.inputs.len() == 2
        && same_origin(&derivation.inputs[0], fact)
        && same_origin(&derivation.inputs[1], baseline)
}

/// A change is the difference between the card value and its baseline, or
/// that difference as a percentage of the baseline.
fn measures_change(derivation: &Derivation, fact: &ValueRef, baseline: &ValueRef, all: &[Derivation]) -> bool {
    differs_from(derivation, fact, baseline)
        || (derivation.op == DerivationOp::Percentage
            && derivation.inputs.len() == 2
            && same_origin(&derivation.inputs[1], baseline)
            && derivation.inputs[0]
                .derivation_id
                .as_deref()
                .and_then(|id| all.iter().find(|d| d.id == id))
                .is_some_and(|difference| differs_from(difference, fact, baseline)))
}

fn value_of(qualifier: &Option<Qualifier>) -> Option<&str> {
    qualifier.as_ref().and_then(|q| q.value.as_deref())
}

/// A valid comparison basis: same measure, and either an earlier period of
/// the same granularity or the same period of another entity.
fn change_basis(scope: &mut Scope, fact: &Fact, baseline: &Fact) -> Option<ChangeBasis> {
    let same_measure = value_of(&fact.unit) == value_of(&baseline.unit)
        && value_of(&fact.currency) == value_of(&baseline.currency)
        && (fact.unit.is_some() || fact.currency.is_some() || (fact.field.is_some() && fact.field == baseline.field));
    let parsed = |fact: &Fact| fact.period.as_ref().and_then(|p| Some((p.key.clone()?, p.granularity?))).filter(|(_, g)| *g != Granularity::Unparsed);
    let ((key, granularity), (base_key, base_granularity)) = (parsed(fact)?, parsed(baseline)?);
    if !same_measure || granularity != base_granularity {
        return None;
    }
    if base_key < key {
        return Some(ChangeBasis::PriorPeriod);
    }
    let mut entity = |fact: &Fact| {
        let result_ref = fact.result_ref.as_deref()?;
        scope.document(result_ref, "").ok()?["subject"]["entity_ref"].as_str().map(str::to_owned)
    };
    let (current, other) = (entity(fact), entity(baseline));
    (base_key == key && current.is_some() && other.is_some() && current != other).then_some(ChangeBasis::OtherEntity)
}

pub(super) fn kpi(block: KpiBlock, scope: &mut Scope) -> Checked<KpiBlock> {
    ensure(!block.cards.is_empty(), BlockIssue::Schema, "cards")?;
    ensure(block.cards.len() <= MAX_KPI_CARDS, BlockIssue::TooManyItems, "cards")?;
    let mut cards = Vec::with_capacity(block.cards.len());
    for (index, card) in block.cards.into_iter().enumerate() {
        let path = format!("cards/{index}");
        scope.item_id(&card.id, &path)?;
        scope.label(&card.label, &format!("{path}/label"))?;
        let fact = scope.fact(&card.fact, &format!("{path}/fact"))?;
        let change = match &card.change {
            None => None,
            // A missing value has nothing to change from.
            Some(_) if fact.value.is_null() => return fail(BlockIssue::ChangeWithoutBasis, format!("{path}/change")),
            Some(change) => {
                let change_path = format!("{path}/change");
                let baseline = scope.fact(&change.baseline, &format!("{change_path}/baseline"))?;
                let (derivation, _) = scope.derivation(&change.derivation_id, &format!("{change_path}/derivation_id"))?;
                let valid = measures_change(derivation, &card.fact.origin(), &change.baseline.origin(), scope.ctx.derivations);
                ensure(valid, BlockIssue::ChangeWithoutBasis, &change_path)?;
                let basis = change_basis(scope, &fact, &baseline);
                let basis = basis.ok_or_else(|| Issue { code: BlockIssue::ChangeWithoutBasis, path: change_path.clone() })?;
                let result = scope.fact(&Fact::derived(change.derivation_id.clone()), &change_path)?;
                Some(KpiChange { derivation_id: change.derivation_id.clone(), baseline, result: Some(result), basis: Some(basis) })
            }
        };
        cards.push(KpiCard { fact, change, ..card });
    }
    Ok(KpiBlock { cards })
}
