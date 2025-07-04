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
});
