// argv synthesis for the generated CLI, shared by the conformance runner
// and the offline body round-trip check.

export const kebab = (s) =>
  s.replace(/_/g, '-').replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();

// One top-level body field as a document on its flag (`--spec={...}`), the
// way the manifest samples it; lists repeat the flag per item.
export function documentArgv(bodyFields) {
  const argv = [];
  for (const f of bodyFields ?? []) {
    const flag = `--${f.cliFlag ?? kebab(f.name)}`;
    if (Array.isArray(f.sample)) {
      for (const item of f.sample) {
        argv.push(`${flag}=${typeof item === 'string' ? item : JSON.stringify(item)}`);
      }
    } else if (typeof f.sample === 'object' && f.sample !== null) {
      argv.push(`${flag}=${JSON.stringify(f.sample)}`);
    } else {
      argv.push(`${flag}=${f.sample}`);
    }
  }
  return argv;
}

// The flattened flags for a body: inputs outside any union, plus those in
// the FIRST arm of every union (nested unions included), and the tag flag
// of each union explicitly. Struct documents whose leaves are also listed
// are skipped so the leaves do the work.
export function flattenedArgv(cli) {
  const firstTag = new Map(cli.unions.map((u) => [u.flag, u.tags[0]]));
  const argv = [];
  const paths = cli.inputs.map((i) => i.path.join('.'));
  const inFirstArms = (input) => input.arms.every((a) => firstTag.get(a.union) === a.tag);
  for (const input of cli.inputs) {
    if (!inFirstArms(input)) continue;
    const dotted = input.path.join('.');
    const hasChildren = paths.some((p) => p !== dotted && (dotted === '' || p.startsWith(dotted + '.')));
    const flag = `--${input.flag}`;
    switch (input.kind) {
      case 'unionTag':
      case 'leaf':
        argv.push(`${flag}=${input.sample}`);
        break;
      case 'kvMap':
        for (const [k, v] of Object.entries(input.sample)) argv.push(`${flag}=${k}=${v}`);
        break;
      case 'scalarList':
        for (const v of input.sample) argv.push(`${flag}=${v}`);
        break;
      case 'shorthandList':
        for (const item of input.sample) {
          const pairs = Object.entries(item).map(([k, v]) => `${kebab(k)}=${v}`);
          // An item with nothing to say is only expressible as a document.
          argv.push(pairs.length > 0 ? `${flag}=${pairs.join(',')}` : `${flag}={}`);
        }
        break;
      case 'docList':
        for (const item of input.sample) argv.push(`${flag}=${JSON.stringify(item)}`);
        break;
      case 'entryDoc':
        argv.push(`${flag}=${JSON.stringify(input.sample)}`);
        break;
      case 'doc':
        if (!hasChildren) argv.push(`${flag}=${JSON.stringify(input.sample)}`);
        break;
      default:
        throw new Error(`unknown cli input kind ${input.kind}`);
    }
  }
  return argv;
}
