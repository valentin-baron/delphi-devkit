import * as assert from 'assert';
import mockfs from 'mock-fs';
import { ProjectLoader } from '../delphiProjects/data/projectLoader';
import { ProjectType } from '../delphiProjects/treeItems/DelphiProject';

suite('ProjectLoader', () => {
  teardown(() => mockfs.restore());

  test('should load projects from config if main file exists', async () => {
    mockfs({
      '/workspace/Project1/Project1.dpr': 'program Project1;'
    });
    const configData = {
      defaultProjects: [
        {
          name: 'Project1',
          type: ProjectType.Application,
          hasDpr: true,
          dprAbsolutePath: '/workspace/Project1/Project1.dpr',
          hasDproj: false,
          hasDpk: false,
          hasExecutable: false,
          hasIni: false
        }
      ]
    };
    const projects = await ProjectLoader.loadProjectsFromConfig(configData);
    assert.ok(projects);
    assert.strictEqual(projects?.length, 1);
    assert.strictEqual(projects?.[0].label, 'Project1');
    assert.strictEqual(projects?.[0].dpr?.fsPath.replace(/\\/g, '/'), '/workspace/Project1/Project1.dpr');
  });

  test('should return null if configData is missing or invalid', async () => {
    const result = await ProjectLoader.loadProjectsFromConfig(undefined);
    assert.strictEqual(result, null);
    const result2 = await ProjectLoader.loadProjectsFromConfig({});
    assert.strictEqual(result2, null);
  });
});
