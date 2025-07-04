import { commands, window } from 'vscode';
import { ProjectDiscovery } from './data/projectDiscovery';

/**
 * Command to benchmark project discovery performance
 */
export async function registerPerformanceCommands() {
  commands.registerCommand('delphi-utils.benchmarkProjectDiscovery', async () => {
    window.showInformationMessage('Starting Delphi project discovery benchmark...');

    const startTime = Date.now();

    try {
      const projects = await ProjectDiscovery.getAllProjects();
      const endTime = Date.now();
      const duration = endTime - startTime;

      const message = `Project discovery completed in ${duration}ms. Found ${projects.length} projects.`;
      window.showInformationMessage(message);
      console.log(`BENCHMARK: ${message}`);

      // Show detailed timing in output console
      console.log('BENCHMARK: Project details:');
      projects.forEach(project => {
        console.log(`  - ${project.label} (${project.projectType})`);
      });

    } catch (error) {
      const endTime = Date.now();
      const duration = endTime - startTime;
      const message = `Project discovery failed after ${duration}ms: ${error}`;
      window.showErrorMessage(message);
      console.error(`BENCHMARK: ${message}`);
    }
  });
}
