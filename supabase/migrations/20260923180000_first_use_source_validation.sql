-- First-use validation of staged source-document contracts.
--
-- Gateway v2 now loads staged (status validated, adapter source-document-v1)
-- contracts as unverified services: the resolver uses them only when no
-- verified service fits, auto mode never runs them, and the plan marks them
-- verified=false. Their first live call settles them here: a usable response
-- promotes the contract with that response as evidence; a second failed call
-- withdraws it (status draft).
set lock_timeout = '5s';

-- catalog.sql now reads these columns.
grant select (status, adapter_version) on catalog.capability_execution_contracts to gateway_v2_runtime;

create table if not exists catalog.source_operation_failures (
  contract_id uuid not null references catalog.capability_execution_contracts(id) on delete cascade,
  failed_at timestamptz not null default now()
);
create index if not exists source_operation_failures_contract on catalog.source_operation_failures(contract_id);
revoke all on catalog.source_operation_failures from public, anon, authenticated;

create or replace function catalog.promote_source_operation(p_contract uuid, p_sample jsonb)
returns boolean language sql security definer set search_path = '' as $$
  with promoted as (
    update catalog.capability_execution_contracts c
       set status = 'active', sample_payload = p_sample, validated_at = now(), updated_at = now(),
           validated_note = 'Promoted by its first usable live response, ' || to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI') || ' UTC'
     where c.id = p_contract and c.status = 'validated' and c.adapter_version = 'source-document-v1'
       and p_sample is not null
       and not exists (select 1 from catalog.capability_execution_contracts x where x.endpoint_id = c.endpoint_id and x.status = 'active')
    returning 1)
  select exists (select 1 from promoted)
$$;

create or replace function catalog.record_source_operation_failure(p_contract uuid)
returns boolean language plpgsql security definer set search_path = '' as $$
declare v_failures integer;
begin
  if not exists (select 1 from catalog.capability_execution_contracts
                  where id = p_contract and status = 'validated' and adapter_version = 'source-document-v1') then
    return false;
  end if;
  insert into catalog.source_operation_failures(contract_id) values (p_contract);
  select count(*) into v_failures from catalog.source_operation_failures where contract_id = p_contract;
  if v_failures >= 2 then
    update catalog.capability_execution_contracts
       set status = 'draft', updated_at = now(), validated_note = 'Withdrawn after ' || v_failures || ' failed live calls'
     where id = p_contract and status = 'validated';
    return true;
  end if;
  return false;
end $$;

revoke all on function catalog.promote_source_operation(uuid, jsonb) from public, anon, authenticated;
revoke all on function catalog.record_source_operation_failure(uuid) from public, anon, authenticated;
grant execute on function catalog.promote_source_operation(uuid, jsonb) to gateway_v2_runtime;
grant execute on function catalog.record_source_operation_failure(uuid) to gateway_v2_runtime;

-- Starting point from the 2026-09-23 treasury validation run: each contract
-- whose test call returned no usable document counts one failure.
insert into catalog.source_operation_failures(contract_id)
select id from catalog.capability_execution_contracts
 where id = any(array['0092cbbd-99ab-44c5-8825-fb062b960235'::uuid,'00feddcf-96ca-4d7e-843e-0aa0bd52ca94'::uuid,'04feec30-03d1-4229-81a4-8b6b44547124'::uuid,'0702b5ed-5925-4a5c-8596-f0405fc8aabe'::uuid,'0973c99f-8edd-4957-8ba5-6ba01c883ec8'::uuid,'0989a02b-1e09-42bc-81b0-fce2d73fe49a'::uuid,'09abf95e-78f5-41ab-8ec7-df553edbbb28'::uuid,'0a7e9ac6-bacd-4202-858e-f1f85a02e2ff'::uuid,'0b4161fc-647c-43e6-8d2a-2bc090e14467'::uuid,'11762fb3-c338-4c7c-818b-e41e1075649d'::uuid,'11b60537-c975-4a1b-8722-f3306f9c427c'::uuid,'1af02cc1-9547-4168-888a-547c6c7971d2'::uuid,'1b0cb8fe-b863-47cb-8687-8157c033c059'::uuid,'1d8ec4da-c1ac-4e4d-8ae1-bd6fc3eeb272'::uuid,'1db989e7-c8ae-4fdd-89af-b9be25abe2d5'::uuid,'1f177f8d-8d05-4e0c-879e-8c0129dd8613'::uuid,'1f4f113e-9cf7-485a-8af8-863d47a42289'::uuid,'21b392ab-87bc-489e-8413-ee1890f614c3'::uuid,'223a485a-4653-4f8d-88da-8a90b87ba490'::uuid,'2315b86a-c156-4774-8d0e-28eb2069d018'::uuid,'2c82e7e2-bf3f-4635-8893-392c9032e53d'::uuid,'315f9ebc-1dcc-4cdc-8c5a-08e6bbc20aa3'::uuid,'31c7687f-f026-4e60-810a-60dba7d6e2ef'::uuid,'32d978f8-3cb6-4f94-8b3a-30f5b69e617a'::uuid,'342a7ca3-82ed-439f-8c85-78475a1ed736'::uuid,'41827864-4524-4866-8870-15b15a7b5e7d'::uuid,'42f95732-6713-43ef-8a90-128328e04c98'::uuid,'46e60bef-1614-4795-8ff1-b0d90689bab9'::uuid,'4a4fe9e2-43ce-40b4-83d3-b362b4a91c3a'::uuid,'58bd16f7-63e4-4b98-871c-64965d6b99ab'::uuid,'6428c8b2-c8d9-4124-8794-d6b390a61efb'::uuid,'66ed1fe2-9020-419b-87d3-4928c8d4d7e4'::uuid,'6a4f5195-0e94-434b-8571-e08f8371e680'::uuid,'82505172-6496-4f8c-85c6-db5ffbc3e877'::uuid,'86bd1e2c-08f2-4d84-8e6d-a1f787fe2599'::uuid,'8702660c-ec49-4a5f-8a26-e575982aaecf'::uuid,'a24e662d-7b2a-40dc-8448-86189eb435a0'::uuid,'a47159d7-5b43-41eb-83ec-b11166f91fbc'::uuid,'a7010a0b-0fa1-45b9-8f27-4436a1645e50'::uuid,'ab2e60b0-1421-4dae-8952-56f545999e3c'::uuid,'ab68233e-8e39-44f4-8402-ec5f933f8f78'::uuid,'abd25fb6-21a2-4163-89dd-edec7cda20db'::uuid,'b49ce461-6c41-4079-8c2a-7993a2f9d70c'::uuid,'b7f44257-c781-481e-87df-6ea983c5c853'::uuid,'ba0725ef-0c8a-4de8-8f88-7a1857087deb'::uuid,'c389f84f-2dd5-4ba2-8de6-3fc421f77fe9'::uuid,'c750c16d-843b-4f2a-8272-5ee4ea474cd0'::uuid,'c9a95c8b-6be8-4307-83e2-d24b521bd00d'::uuid,'cc79d461-d263-45d5-8391-3c3fff7334d2'::uuid,'cd9caaad-1712-4a30-84b5-79ce03c9a7fc'::uuid,'d1a864fb-11fd-47fc-894f-205d6ee3e3dd'::uuid,'d4d1d582-54ec-463d-8261-a529bd4795dd'::uuid,'dcfa3752-0b89-4d68-874c-c3d3e70f30f8'::uuid,'dd127a0f-40fd-4073-82ab-a18fb6cb6fe8'::uuid,'dd8dc662-926f-4a8f-8ec5-42508c0b96ec'::uuid,'dfbf83bf-2074-4a57-8fcd-5ab322733a4d'::uuid,'e1548604-c2e6-4d6b-854b-60a04ea6f4d9'::uuid,'e1c92a7d-3898-491c-86d7-9ea5e3498d51'::uuid,'e24cb6c9-f871-426e-8f1d-bc846e7246ed'::uuid,'ee220816-698e-4b2f-88f1-41599763bb92'::uuid,'f7968d2c-8c44-40b3-813c-05c39a8cfa20'::uuid,'f89aab95-9d95-418b-872e-bca8b1e3fe44'::uuid,'fab0c37d-9853-4238-87af-5783e4e32cb2'::uuid,'fbf8a039-cf5c-4699-81e2-dbe4f5f5e68e'::uuid,'ff217449-87ba-4355-8e40-e15e0de1dbf5'::uuid]) and status = 'validated';

-- AgentBodega never confirms a payment receipt: every call stays held.
update catalog.capability_execution_contracts
   set status = 'draft', updated_at = now(), validated_note = 'Withdrawn: provider never confirms payment receipts (2026-09-23 validation)'
 where id = any(array['1bab4f61-0565-420f-81f7-41de67c5f500'::uuid,'2d3a7fcb-7f46-4fbe-82d2-db7506cadf06'::uuid,'3f75dc78-913f-4c96-89c5-069b0b5b4b99'::uuid,'79d2f2db-8c54-40ab-803a-91e70897f8e9'::uuid,'9e37b326-0937-4c2d-8f30-659f9277e375'::uuid,'a98bb08d-604e-47b9-8dfd-887cd1391cc6'::uuid,'d2c4ef37-04f6-4eeb-888e-0644b5dae1aa'::uuid,'d44d6802-98d5-4ee2-886d-e2013882b12e'::uuid,'7bd374a4-8d48-49d1-813b-7b67a40ba00e'::uuid,'6d49e99f-8018-49d9-84de-f573763a37f4'::uuid,'865c43cf-7994-435c-83ef-48499985bab7'::uuid,'c4938032-1848-4b75-851e-9e7e798f1de2'::uuid,'f4db98cf-3329-4105-804e-2c94f066d679'::uuid]) and status = 'validated';
