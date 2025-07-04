import * as assert from 'assert';
import * as path from 'path';
import mockfs from 'mock-fs';
import { Uri } from 'vscode';
import { DelphiProjectUtils } from '../delphiProjects/utils';

suite('DelphiProjectUtils', () => {
  teardown(() => {
    mockfs.restore();
  });

  test('should remove BOM from string', () => {
    // @ts-ignore: test private method
    const result = DelphiProjectUtils["removeBOM"]('\uFEFFHello');
    assert.strictEqual(result, 'Hello');
  });

  test('should find Dpr from Dproj (case-insensitive)', async () => {
    mockfs({
      '/workspace/Project1/Project1.dpr': 'program Project1;',
      '/workspace/Project1/Project1.dproj': '<Project></Project>'
    });
    const dprojUri = Uri.file('/workspace/Project1/Project1.dproj');
    const dprUri = await DelphiProjectUtils.findDprFromDproj(dprojUri);
    assert.ok(dprUri);
    // Normalize both paths for cross-platform compatibility
    const actual = dprUri?.fsPath.replace(/\\/g, '/');
    const expected = '/workspace/Project1/Project1.dpr';
    assert.strictEqual(actual, expected);
  });

  test('should find executable from Dproj (regex path)', async () => {
    mockfs({
      '/workspace/Project2/Project2.dproj': '<Project><PropertyGroup><DCC_DependencyCheckOutputName>bin/Project2.exe</DCC_DependencyCheckOutputName></PropertyGroup></Project>',
      '/workspace/Project2/bin/Project2.exe': ''
    });
    const dprojUri = Uri.file('/workspace/Project2/Project2.dproj');
    const exeUri = await DelphiProjectUtils.findExecutableFromDproj(dprojUri);
    assert.ok(exeUri);
    const actual = exeUri?.fsPath.replace(/\\/g, '/');
    const expected = '/workspace/Project2/bin/Project2.exe';
    assert.strictEqual(actual, expected);
  });

  test('should find Dproj from Dpr (case-insensitive)', async () => {
    mockfs({
      '/workspace/Project3/Project3.DPROJ': '<Project></Project>',
      '/workspace/Project3/Project3.dpr': 'program Project3;'
    });
    const dprUri = Uri.file('/workspace/Project3/Project3.dpr');
    const dprojUri = await DelphiProjectUtils.findDprojFromDpr(dprUri);
    assert.ok(dprojUri);
    const actual = dprojUri?.fsPath.replace(/\\/g, '/');
    const expected = '/workspace/Project3/Project3.DPROJ';
    assert.strictEqual(actual, expected);
  });

  test('should find project files from executable', async () => {
    mockfs({
      '/workspace/Project4/Project4.exe': '',
      '/workspace/Project4/Project4.dpr': 'program Project4;',
      '/workspace/Project4/Project4.dproj': '<Project></Project>'
    });
    const exeUri = Uri.file('/workspace/Project4/Project4.exe');
    const result = await DelphiProjectUtils.findProjectFromExecutable(exeUri);
    assert.ok(result.dpr);
    assert.ok(result.dproj);
    assert.strictEqual(result.dpr?.fsPath.replace(/\\/g, '/'), '/workspace/Project4/Project4.dpr');
    assert.strictEqual(result.dproj?.fsPath.replace(/\\/g, '/'), '/workspace/Project4/Project4.dproj');
  });

  test('should find Dpk from Dproj (case-insensitive)', async () => {
    mockfs({
      '/workspace/Project5/Project5.dproj': '<Project></Project>',
      '/workspace/Project5/Project5.dpk': ''
    });
    const dprojUri = Uri.file('/workspace/Project5/Project5.dproj');
    const dpkUri = await DelphiProjectUtils.findDpkFromDproj(dprojUri);
    assert.ok(dpkUri);
    const actual = dpkUri?.fsPath.replace(/\\/g, '/');
    const expected = '/workspace/Project5/Project5.dpk';
    assert.strictEqual(actual, expected);
  });
});

import { DelphiProject, ProjectType } from '../delphiProjects/treeItems/DelphiProject';

suite('DelphiProject icon logic', () => {
  function getThemeIconId(iconPath: any): string | undefined {
    return iconPath && typeof iconPath === 'object' && 'id' in iconPath ? iconPath.id : undefined;
  }
  test('uses package icon if DPK is present', () => {
    const project = new DelphiProject('Test', ProjectType.Application);
    project.dpk = Uri.file('dummy.dpk');
    project.setIcon();
    assert.strictEqual(getThemeIconId(project.iconPath), 'package');
  });
  test('uses run icon if DPR is present and no DPK', () => {
    const project = new DelphiProject('Test', ProjectType.Application);
    project.dpr = Uri.file('dummy.dpr');
    project.setIcon();
    assert.strictEqual(getThemeIconId(project.iconPath), 'run');
  });
  test('uses library icon if type is Library and no DPK/DPR', () => {
    const project = new DelphiProject('Test', ProjectType.Library);
    project.setIcon();
    assert.strictEqual(getThemeIconId(project.iconPath), 'library');
  });
  test('uses fallback icon if no DPK/DPR and not library', () => {
    const project = new DelphiProject('Test', ProjectType.Application);
    project.setIcon();
    assert.strictEqual(getThemeIconId(project.iconPath), 'symbol-class');
  });
});
