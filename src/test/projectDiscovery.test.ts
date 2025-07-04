import * as assert from 'assert';
import mockfs from 'mock-fs';
import * as vscode from 'vscode';
import { ProjectDiscovery } from '../delphiProjects/data/projectDiscovery';

suite('ProjectDiscovery', () => {
  teardown(() => mockfs.restore());

  test('should return empty array if no workspace folders', async () => {
    // Save original getter
    const originalFolders = Object.getOwnPropertyDescriptor(vscode.workspace, 'workspaceFolders');
    // Redefine property to return undefined
    Object.defineProperty(vscode.workspace, 'workspaceFolders', {
      get: () => undefined,
      configurable: true
    });
    const projects = await ProjectDiscovery.getAllProjects();
    assert.deepStrictEqual(projects, []);
    // Restore original property
    if (originalFolders) {
      Object.defineProperty(vscode.workspace, 'workspaceFolders', originalFolders);
    }
  });

  // Note: Full integration test for getAllProjects would require VS Code API mocking for workspace.findFiles, getConfiguration, etc.
});
