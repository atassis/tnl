import {mkdir, writeFile} from 'node:fs/promises';
import {cargo, frontendRequire} from './process.mjs';

const Ajv = frontendRequire('ajv').default;
const standaloneCode = frontendRequire('ajv/dist/standalone/index.js').default;
const {compile} = frontendRequire('json-schema-to-typescript');
const directory = new URL('../frontend/inspector/src/api/generated/', import.meta.url);
const schemas = JSON.parse(cargo(['run','--quiet','--locked','-p','tnl-inspector-api','--bin','export-schema']));
const fixtures = JSON.parse(cargo(['run','--quiet','--locked','-p','tnl-inspector-api','--example','fixtures']));
const ajv = new Ajv({strict:true, code:{source:true, esm:true}, allErrors:false});
const exports = {};
let declarations = '';
await mkdir(directory, {recursive:true});
for (const [name, schema] of Object.entries(schemas)) {
  const id = `tnl-inspector:${name}`;
  ajv.addSchema({...schema, $id:id}, id);
  exports[`validate_${name}`] = id;
  const validate = ajv.getSchema(id);
  if (!validate(fixtures[name])) throw new Error(`Rust fixture invalid for ${name}: ${ajv.errorsText(validate.errors)}`);
  const types = await compile(schema, schema.title, {bannerComment:'/* Generated from Rust DTOs; do not edit. */', unknownAny:true});
  await writeFile(new URL(`${name}.ts`, directory), types);
  declarations += `export function validate_${name}(value: unknown): value is import('./${name}').${schema.title};\n`;
}
await writeFile(new URL('validators.js', directory), standaloneCode(ajv, exports));
await writeFile(new URL('validators.d.ts', directory), declarations);
await writeFile(new URL('schemas.json', directory), JSON.stringify(schemas, null, 2)+'\n');
await writeFile(new URL('fixtures.json', directory), JSON.stringify(fixtures, null, 2)+'\n');
