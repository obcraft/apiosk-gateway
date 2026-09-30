import React, { useState } from "react";
import { DataComponent, DataTable, MetricCard, ReportSection, RichNarrative, useDataApp } from "../../data-app-public.jsx";
import "./source-audit.css";

const col=(key,label)=>({key,label});
const sourceColumns=[col("name","Officiële bron"),col("country","Land"),col("delivery","API / datasettoegang"),col("cost_status","Bronkosten"),col("data_offer","Gegevens")];
const countryColumns=[col("country","EU-land"),col("source_count","Conceptbronnen"),col("sources","Alle officiële conceptbronnen"),col("free_sources","Bronnen met gratis onderdeel")];
function Pick({label,value,onChange,options}) {
  return <label className="audit-pick">{label}<select aria-label={label} value={value} onChange={e=>onChange(e.target.value)}>{options.map(([v,l])=><option key={v} value={v}>{l}</option>)}</select></label>;
}
function EvidenceLinks({urls}) {
  return <ul className="audit-links" data-reviewed-rows>{urls.filter(Boolean).map(url=><li key={url}><a href={url} target="_blank" rel="noreferrer">{new URL(url).hostname} · {decodeURI(new URL(url).pathname).split('/').filter(Boolean).slice(-1)[0]||'documentatie'}</a></li>)}</ul>;
}
export function ReportContent() {
  const {snapshot,appTitle,setAppTitle,canEdit,mode,visible}=useDataApp();
  const rows=snapshot.queries.source_inventory.rows;
  const countries=snapshot.queries.countries.rows;
  const totals=snapshot.queries.inventory_totals.rows[0];
  const [view,setView]=useState('sources');
  const [cost,setCost]=useState('');
  const [country,setCountry]=useState('');
  const [delivery,setDelivery]=useState('');
  const [selectedId,setSelectedId]=useState('fr-dinum-recherche');
  const filtered=rows.filter(r=>(!country||r.country_codes.includes(country))&&(!cost||(cost==='free'?r.zero_cost_scope_confirmed:r.cost_code===cost))&&(!delivery||(delivery==='downloads'?r.delivery.includes('downloads')||r.delivery.startsWith('Downloads'):delivery==='api'?r.delivery.includes('API'):r.delivery==='Nog te bevestigen')));
  const selected=filtered.find(r=>r.id===selectedId)||filtered[0];
  const filteredCountryIds=new Set(filtered.flatMap(r=>r.country_codes));
  const countryRows=countries.filter(c=>filteredCountryIds.has(c.id));
  const sourceForSelectedCountry=countries.find(c=>c.id===country);
  const reset=()=>{setCost('');setCountry('');setDelivery('');};
  return <article className="report-content audit-report" aria-label="Officiële EU native conceptbronnen">
    <header className="report-hero">
      <div className="audit-eyebrow">EU27 · OFFICIËLE NATIONALE BRONNEN · CONCEPT</div>
      <h1 data-data-app-title contentEditable={canEdit&&mode==='edit'} suppressContentEditableWarning onBlur={canEdit&&mode==='edit'?e=>setAppTitle(e.currentTarget.textContent.trim()||appTitle):undefined}>{appTitle}</h1>
      <RichNarrative id="eu-audit:intro" className="report-deck" value="**Open bedrijfsdata is beschikbaar in veel EU-landen.** Frankrijk, België, Estland, Letland en Ierland bieden bijvoorbeeld registerdatasets of jaarrekeningdownloads. Finland, Zweden en Slowakije ontsluiten ook openbare datasets via API’s.\n\nBinnen deze selectie hebben **20 van de 41 officiële conceptbronnen een bevestigd gratis onderdeel**, verdeeld over 13 EU-landen." />
    </header>
    <div className="audit-metrics">
      <MetricCard id="eu-audit-free" title="Met gratis onderdeel" queryId="inventory_totals" sourceRows={[totals]} displayRows={[totals]} value={totals.zero_cost_sources} description="€0 bronkosten voor de genoemde scope; registratie en voorwaarden kunnen gelden." />
      <MetricCard id="eu-audit-commercial" title="Commerciële producten" queryId="inventory_totals" sourceRows={[totals]} displayRows={[totals]} value={totals.commercial_sources} description="Betaalde producten of contracten; geen blijvend gratis gebruik bevestigd." />
      <MetricCard id="eu-audit-unknown" title="Nog te bevestigen" queryId="inventory_totals" sourceRows={[totals]} displayRows={[totals]} value={totals.unknown_sources} description="Machine-toegang, tarief of hergebruik nog onvoldoende bevestigd." />
    </div>
    <ReportSection id="eu-audit-scope" title="Scope en status" queryId="source_inventory" sourceRows={rows} showHeading={false}>
      <RichNarrative id="eu-audit:scope" value="**Land, open data en toegang zijn verschillende eigenschappen.** Een Frans bedrijfsregister kan tegelijk een nationale bron, een open dataset én een API zijn. Een API is een manier om gegevens op te vragen; downloads zijn een andere manier. De tabel maakt dat per bron zichtbaar.\n\nAlle bronnen zijn conceptintegraties voor Apiosk. Gratis betekent €0 bronkosten voor het genoemde onderdeel, met eventuele registratie en gebruiksvoorwaarden. Onderzoekspeildatum: 30 september 2026. Bij onbevestigde toegang of rechten blijft de status open." />
    </ReportSection>
    <nav className="audit-nav" aria-label="Overzicht kiezen">
      <button aria-pressed={view==='sources'} onClick={()=>setView('sources')}>41 conceptbronnen</button>
      <button aria-pressed={view==='countries'} onClick={()=>setView('countries')}>27 EU-landen</button>
      <button className="audit-reset" onClick={reset}>Wis filters</button>
    </nav>
    <div className="audit-controls">
      <Pick label="Kosten" value={cost} onChange={setCost} options={[["","Alle kostenstatussen"],["free","Alleen gratis onderdelen"],["commercial","Commerciële producten"],["unknown","Nog te bevestigen"]]}/>
      <Pick label="EU-land" value={country} onChange={setCountry} options={[["","Alle 27 EU-landen"],...countries.map(c=>[c.id,c.country])]} />
      <Pick label="Aanbod" value={delivery} onChange={setDelivery} options={[["","Alle toegangsvormen"],["downloads","Met datasetdownloads"],["api","Met API"],["unknown","Nog te bevestigen"]]}/>
    </div>
    {view==='sources'&&<>
      {visible('audit-source-table')&&<DataComponent id="audit-source-table" title={`EU-conceptbronnen · ${filtered.length} van 41${sourceForSelectedCountry?' · '+sourceForSelectedCountry.country:''}`} queryId="source_inventory" kind="table" sourceRows={filtered} displayRows={filtered}>
        <DataTable key={`${cost}-${country}-${delivery}`} rows={filtered} columns={sourceColumns} caption="Officiële nationale EU-conceptbronnen" rowKey="id" selectedRowKey={selected?.id} onRowSelect={r=>setSelectedId(r.id)} rowActionLabel={r=>`Bekijk ${r.name}`}/>
      </DataComponent>}
      {selected&&visible('audit-source-detail')&&<DataComponent id="audit-source-detail" title={selected.name} queryId="source_inventory" kind="table" sourceRows={[selected]} displayRows={[selected]} variant="card">
        <div className="audit-detail" data-reviewed-rows>
          <div className="audit-detail-meta"><span>{selected.country}</span><span>Conceptintegratie</span><strong>{selected.cost_status}</strong></div>
          <dl><dt>Gegevens</dt><dd>{selected.data_offer}</dd><dt>Aanbod</dt><dd>{selected.delivery}</dd><dt>Gratis gegevens / acties</dt><dd>{selected.free_scope||'Gratis machinegebruik nog niet bevestigd.'}</dd>
            <dt>Toegang</dt><dd>{selected.access_kind}</dd><dt>Voorwaarden</dt><dd>{selected.conditions}</dd>
            {selected.excluded_scope&&<><dt>Overige producten</dt><dd>{selected.excluded_scope}</dd></>}
          </dl>
          <details><summary>{selected.proposed_actions.length} voorgestelde Apiosk-acties</summary>
            <ul className="audit-actions">{selected.proposed_actions.map(a=><li key={a.public_name}><strong>{a.public_name}</strong><div>Invoer: {a.inputs.join(' · ')}</div><div>Uitvoer: {a.outputs.join(' · ')}</div></li>)}</ul>
            <p>Voorgestelde contracten; de gratis scope hierboven bepaalt welke onderdelen zonder bronkosten kunnen worden geïntegreerd.</p>
          </details>
          <details><summary>Wat nodig is voor integratie</summary><ul>{selected.required_work.map(w=><li key={w}>{w}</li>)}</ul><p>Daarna zijn een passende adapter, gevalideerd uitvoeringscontract en succesvolle Apiosk-uitvoering nodig.</p></details>
          <div className="audit-label">Officiële onderbouwing</div><EvidenceLinks urls={selected.evidence_urls}/>
        </div>
      </DataComponent>}
    </>}
    {view==='countries'&&visible('audit-country-table')&&<DataComponent id="audit-country-table" title={`EU-landen · ${countryRows.length} van 27`} queryId="countries" kind="table" sourceRows={countryRows} displayRows={countryRows}>
      <RichNarrative id="eu-audit:country-guide" value="De selectie toont landen met minstens één bron die aan je kostenfilter voldoet. Iedere landenrij bevat het volledige onderzochte bronnenpakket voor dat land. Klik op een land voor de afzonderlijke bronnen."/>
      <DataTable key={`${cost}-${country}`} rows={countryRows} columns={countryColumns} caption="Officiële conceptbronnen per EU-land" rowKey="id" onRowSelect={c=>{setCountry(c.id);setView('sources');setSelectedId(c.source_ids[0]);}} rowActionLabel={c=>`Bronnen voor ${c.country}`}/>
    </DataComponent>}
  </article>;
}
