import * as assert from 'assert';
import { readFileSync } from 'fs';
import { join } from 'path';
import {
  compileOutcomeOf,
  configurationName,
  contributesDebugger,
  isDebugTarget,
  linkToCompile,
  projectReference,
  projectReferredTo
} from '../debug/contract';

/** The sample `ddk-core` serializes its `DebugTarget` to, checked there against the Rust struct. */
function sampleTarget(): Record<string, unknown> {
  const path = join(__dirname, '..', '..', '..', 'core', 'tests', 'fixtures', 'debug_target.sample.json');
  return JSON.parse(readFileSync(path, 'utf8'));
}

suite('Debug contract', () => {
  suite('compile outcome', () => {
    test('is read from a reply that carries one', () => {
      assert.deepStrictEqual(compileOutcomeOf({ success: true, cancelled: false }), { success: true, cancelled: false });
      assert.deepStrictEqual(compileOutcomeOf({ success: false, cancelled: true }), { success: false, cancelled: true });
      assert.deepStrictEqual(compileOutcomeOf({ success: false }), { success: false, cancelled: false });
    });

    test('is unknown for the reply of a server that reports none', () => {
      for (const reply of [null, undefined, '', 0, [], {}, { success: 'true' }, { cancelled: true }])
        assert.strictEqual(compileOutcomeOf(reply), undefined, JSON.stringify(reply));
    });
  });

  suite('debug target', () => {
    test('accepts what ddk-core serializes', () => {
      assert.ok(isDebugTarget(sampleTarget()));
    });

    test('names every field of the interface', () => {
      const expected = [
        'project_id', 'project', 'project_file', 'main_source', 'kind', 'executable', 'host_application',
        'compiler', 'config', 'platform', 'bitness', 'symbols', 'source_root', 'source_search_paths',
        'modules', 'args', 'warnings', 'notes'
      ];
      assert.deepStrictEqual(Object.keys(sampleTarget()).sort(), expected.sort());
    });

    test('rejects a reply with a field missing or of another type', () => {
      for (const field of Object.keys(sampleTarget())) {
        const incomplete = sampleTarget();
        delete incomplete[field];
        assert.ok(!isDebugTarget(incomplete), `without ${field}`);
      }
      const mistyped: [string, unknown][] = [
        ['kind', 'Package'],
        ['symbols', { map: 'C:/x.map' }],
        ['modules', [{ name: 'x.bpl', binary: 1, map: null, rsm: null, dcp: null }]],
        ['warnings', 'none'],
        ['bitness', '64']
      ];
      for (const [field, value] of mistyped)
        assert.ok(!isDebugTarget({ ...sampleTarget(), [field]: value }), `${field} = ${JSON.stringify(value)}`);
      for (const reply of [null, undefined, 'target', []]) assert.ok(!isDebugTarget(reply));
    });
  });

  suite('debugger detection', () => {
    test('finds a contributed debugger of the type', () => {
      const manifest = { contributes: { debuggers: [{ type: 'delphi-win64' }, { type: 'delphi', label: 'Delphi' }] } };
      assert.ok(contributesDebugger(manifest, 'delphi'));
      assert.ok(!contributesDebugger(manifest, 'lldb'));
    });

    test('says no to any other shape of manifest', () => {
      const manifests = [
        undefined, null, 'manifest', {}, { contributes: null }, { contributes: {} },
        { contributes: { debuggers: { type: 'delphi' } } },
        { contributes: { debuggers: [null, 'delphi', { type: 7 }] } }
      ];
      for (const manifest of manifests) assert.ok(!contributesDebugger(manifest, 'delphi'), JSON.stringify(manifest));
    });
  });

  suite('project references', () => {
    const app = { id: 1, name: 'App' };
    const twin = { id: 2, name: 'Twin' };
    const otherTwin = { id: 3, name: 'twin' };
    const all = [app, twin, otherTwin];

    test('use the name when it is unique and the id when it is not', () => {
      assert.strictEqual(projectReference(app, all), 'App');
      assert.strictEqual(projectReference(twin, all), '2');
      assert.strictEqual(projectReference(otherTwin, all), '3');
    });

    test('never list one label twice', () => {
      const names = all.flatMap((project) => [
        configurationName('launch', project, all),
        configurationName('attach', project, all)
      ]);
      assert.deepStrictEqual(names, [
        'Debug App (DDK)', 'Attach to App (DDK)',
        'Debug Twin #2 (DDK)', 'Attach to Twin #2 (DDK)',
        'Debug twin #3 (DDK)', 'Attach to twin #3 (DDK)'
      ]);
      assert.strictEqual(new Set(names).size, names.length);
    });

    test('resolve back to the project they were made from', () => {
      for (const project of all) assert.strictEqual(projectReferredTo(projectReference(project, all), all), project);
    });

    test('resolve by id, by unique name in any casing, and not otherwise', () => {
      assert.strictEqual(projectReferredTo(2, all), twin);
      assert.strictEqual(projectReferredTo(' 3 ', all), otherTwin);
      assert.strictEqual(projectReferredTo('app', all), app);
      for (const reference of ['Twin', 'Nowhere', '99', '', undefined, null, {}, true])
        assert.strictEqual(projectReferredTo(reference, all), undefined, JSON.stringify(reference));
    });

    test('prefer the id over a project named like a number', () => {
      const numbered = [{ id: 5, name: '7' }, { id: 7, name: 'Seven' }];
      assert.strictEqual(projectReferredTo('7', numbered), numbered[1]);
    });
  });

  suite('link to compile', () => {
    const links = [{ id: 10 }, { id: 20 }];

    test('is the one the user acted on', () => {
      assert.strictEqual(linkToCompile(links, 20), links[1]);
    });

    test('is the first one when none, or a foreign one, was picked', () => {
      assert.strictEqual(linkToCompile(links), links[0]);
      assert.strictEqual(linkToCompile(links, 99), links[0]);
    });

    test('does not exist for a project without links', () => {
      assert.strictEqual(linkToCompile([], 10), undefined);
    });
  });
});
