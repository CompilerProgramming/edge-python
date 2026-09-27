alter table user add column quota integer;

create table vacated (
  handle text primary key,
  left_by text,
  vacated_at integer not null
) strict;
