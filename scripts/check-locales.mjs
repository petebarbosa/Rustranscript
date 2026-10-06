// Confere os 3 idiomas contra o en-US (a referência): mesmas chaves, mesmos {placeholders},
// plural `_one` sempre com `_other`, sem valor vazio. Sai com código 1 se algo estiver fora.
import { readFileSync } from 'node:fs'

const DIR = new URL('../ui/src/locales/', import.meta.url)
const REF = 'en-US'
const load = l => JSON.parse(readFileSync(new URL(`${l}.json`, DIR), 'utf8'))
const ph = s => [...s.matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort().join(',')

const ref = load(REF)
const errors = []
for (const lang of ['pt-BR', 'es-419', REF]) {
  const d = load(lang)
  for (const k of Object.keys(ref)) if (!(k in d)) errors.push(`${lang}: missing key ${k}`)
  for (const k of Object.keys(d)) {
    if (!(k in ref)) errors.push(`${lang}: extra key ${k}`)
    else if (typeof d[k] !== 'string' || !d[k].trim()) errors.push(`${lang}: empty value ${k}`)
    else if (ph(d[k]) !== ph(ref[k])) errors.push(`${lang}: placeholders of ${k} differ ({${ph(d[k])}} vs {${ph(ref[k])}})`)
    if (k.endsWith('_one') && !(k.slice(0, -4) + '_other' in d)) errors.push(`${lang}: ${k} without _other`)
  }
}
if (errors.length) {
  console.error(errors.join('\n'))
  process.exit(1)
}
console.log(`locales ok (${Object.keys(ref).length} keys x 3)`)
