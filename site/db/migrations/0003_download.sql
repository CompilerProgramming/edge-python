create table download (
  package text not null references package(name),
  visitor text not null,
  day text not null,
  primary key (package, visitor, day)
) strict;
