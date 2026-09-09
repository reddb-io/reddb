# Collection expressions

Run each `.rql` file through the public RQL query interface. These are small schema
consumers for the three planned applications, not complete multimodel workflows.
No provider calls or application-specific engine extensions are required.

Stored fields use `GENERATED ALWAYS AS (...) STORED`; column validations use
`CHECK (...)`. Generated inputs are recomputed from the final row. CHECK accepts
NULL, so use NOT NULL where a value is mandatory. Functions and virtual generated
fields are not supported by this first slice. See the implementation ledger in
`docs/architecture/multimodel-building-block-program.md` for outstanding work.
