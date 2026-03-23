import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as https from 'https';
import * as vscode from 'vscode';
import * as tar from 'tar';
import extractZip from 'extract-zip';
import {
   LanguageClient,
   LanguageClientOptions,
   ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

type PlatformAsset = {
   archiveName: string;
   binaryName: string;
   archiveType: 'zip' | 'tar.gz';
};

type InstalledMetadata = {
   version: string;
   archiveName: string;
   binaryName: string;
};

const STORAGE_DIR_NAME = 'wa2lsp';
const METADATA_FILE_NAME = 'metadata.json';

export async function activate(context: vscode.ExtensionContext) {
   const outputChannel = vscode.window.createOutputChannel('WA2 Extension');
   context.subscriptions.push(outputChannel);

   const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
   if (workspaceRoot) {
      outputChannel.appendLine(`WA2: Workspace root: ${workspaceRoot}`);
   }

   let serverCommand: string;

   try {
      const configuredServerPath = await resolveConfiguredServerPath(outputChannel);

      if (configuredServerPath) {
         serverCommand = configuredServerPath;
         outputChannel.appendLine(`WA2: Using configured server path ${serverCommand}`);
      } else {
         serverCommand = await resolveServerCommand(context, outputChannel);
         outputChannel.appendLine(`WA2: Using managed LSP server at ${serverCommand}`);
      }
   } catch (err) {
      const msg = `WA2: Failed to prepare language server: ${formatError(err)}`;
      outputChannel.appendLine(msg);
      void vscode.window.showErrorMessage(msg);
      throw err;
   }

   const serverOptions: ServerOptions = {
      command: serverCommand,
      args: ['--serve'],
      options: {
         cwd: workspaceRoot,
      },
   };

   const clientOptions: LanguageClientOptions = {
      documentSelector: [
         { language: 'cloudformation-yaml', scheme: 'file' },
         { language: 'cloudformation-json', scheme: 'file' },
      ],
      synchronize: {
         fileEvents: [
            vscode.workspace.createFileSystemWatcher('**/*.{yml,yaml,json,template,cfn}'),
            vscode.workspace.createFileSystemWatcher('**/wa2.toml'),
            vscode.workspace.createFileSystemWatcher('**/*.wa2'),
         ],
      },
   };

   client = new LanguageClient(
      'wa2lsp',
      'WA2 LSP',
      serverOptions,
      clientOptions
   );

   client.start().then(() => {
      outputChannel.appendLine('WA2: Language server started successfully');
   }).catch(err => {
      const msg = formatError(err);
      outputChannel.appendLine(`WA2: Failed to start language server: ${msg}`);
      void vscode.window.showErrorMessage(`WA2: Failed to start language server: ${msg}`);
   });

   context.subscriptions.push({
      dispose: () => client?.stop()
   });
}

async function resolveConfiguredServerPath(
   outputChannel: vscode.OutputChannel
): Promise<string | undefined> {
   const config = vscode.workspace.getConfiguration('wa2');
   const configuredPath = config.get<string>('serverPath')?.trim();

   if (!configuredPath) {
      return undefined;
   }

   const expandedPath = expandHomeDir(configuredPath);

   if (!path.isAbsolute(expandedPath)) {
      throw new Error(`wa2.serverPath must be an absolute path: ${configuredPath}`);
   }

   if (!await pathExists(expandedPath)) {
      throw new Error(`wa2.serverPath does not exist: ${expandedPath}`);
   }

   outputChannel.appendLine(`WA2: Found configured local server path ${expandedPath}`);
   return expandedPath;
}

function expandHomeDir(inputPath: string): string {
   if (inputPath === '~') {
      return os.homedir();
   }

   if (inputPath.startsWith(`~${path.sep}`)) {
      return path.join(os.homedir(), inputPath.slice(2));
   }

   return inputPath;
}

export async function deactivate(): Promise<void> {
   if (!client) {
      return;
   }
   await client.stop();
}

async function resolveServerCommand(
   context: vscode.ExtensionContext,
   outputChannel: vscode.OutputChannel
): Promise<string> {
   const extensionVersion = context.extension.packageJSON.version as string;
   const platformAsset = getPlatformAsset();
   const storageDir = path.join(context.globalStorageUri.fsPath, STORAGE_DIR_NAME);
   const binaryPath = path.join(storageDir, platformAsset.binaryName);
   const metadataPath = path.join(storageDir, METADATA_FILE_NAME);

   await fs.promises.mkdir(storageDir, { recursive: true });

   const existingMetadata = await readInstalledMetadata(metadataPath);
   if (
      existingMetadata?.version === extensionVersion &&
      existingMetadata.archiveName === platformAsset.archiveName &&
      existingMetadata.binaryName === platformAsset.binaryName &&
      await pathExists(binaryPath)
   ) {
      outputChannel.appendLine(
         `WA2: Found managed wa2lsp ${extensionVersion} in extension storage`
      );
      return binaryPath;
   }

   const releaseUrl = constructLspDownloadUrl(context, extensionVersion, platformAsset);

   outputChannel.appendLine(
      `WA2: Managed wa2lsp ${extensionVersion} not installed; downloading from ${releaseUrl}`
   );

   await installServerRelease({
      extensionVersion,
      platformAsset,
      releaseUrl,
      storageDir,
      metadataPath,
      outputChannel,
   });

   return binaryPath;
}

function constructLspDownloadUrl(context: vscode.ExtensionContext, extensionVersion: string, platformAsset: PlatformAsset): string {
   const repoField = context.extension.packageJSON.repository;

   let repoUrl: string | undefined;

   if (typeof repoField === 'string') {
      repoUrl = repoField;
   } else if (repoField && typeof repoField.url === 'string') {
      repoUrl = repoField.url;
   }

   if (!repoUrl) {
      throw new Error('package.json repository.url is missing or invalid');
   }

   repoUrl = repoUrl
      .replace(/^git\+/, '')
      .replace(/\.git$/, '');

   const normalizedRepo = repoUrl.endsWith('/') ? repoUrl.slice(0, -1) : repoUrl;
   const releaseUrl = `${normalizedRepo}/releases/download/v${extensionVersion}/${platformAsset.archiveName}`;
   return releaseUrl;
}

function getPlatformAsset(): PlatformAsset {
   const isWin = process.platform === 'win32';
   const isMac = process.platform === 'darwin';
   const isLinux = process.platform === 'linux';
   const isX64 = process.arch === 'x64';
   const isArm64 = process.arch === 'arm64';

   if (isLinux && isX64) {
      return {
         archiveName: 'wa2lsp-linux-x64.tar.gz',
         binaryName: 'wa2lsp',
         archiveType: 'tar.gz',
      };
   }

   if (isMac && isX64) {
      return {
         archiveName: 'wa2lsp-darwin-x64.tar.gz',
         binaryName: 'wa2lsp',
         archiveType: 'tar.gz',
      };
   }

   if (isMac && isArm64) {
      return {
         archiveName: 'wa2lsp-darwin-arm64.tar.gz',
         binaryName: 'wa2lsp',
         archiveType: 'tar.gz',
      };
   }

   if (isWin && isX64) {
      return {
         archiveName: 'wa2lsp-win32-x64.zip',
         binaryName: 'wa2lsp.exe',
         archiveType: 'zip',
      };
   }

   throw new Error(`Unsupported platform: ${process.platform} ${process.arch}`);
}

async function installServerRelease(args: {
   extensionVersion: string;
   platformAsset: PlatformAsset;
   releaseUrl: string;
   storageDir: string;
   metadataPath: string;
   outputChannel: vscode.OutputChannel;
}): Promise<void> {
   const { extensionVersion, platformAsset, releaseUrl, storageDir, metadataPath, outputChannel } = args;

   const archivePath = path.join(storageDir, platformAsset.archiveName);

   outputChannel.appendLine(`WA2: Downloading LSP to ${archivePath}`);

   await downloadFile(releaseUrl, archivePath);

   await extractArchive({
      archivePath,
      archiveType: platformAsset.archiveType,
      destinationDir: storageDir,
      outputChannel,
   });

   const binaryPath = path.join(storageDir, platformAsset.binaryName);
   if (!await pathExists(binaryPath)) {
      throw new Error(`Downloaded archive did not contain ${platformAsset.binaryName}`);
   }

   if (process.platform !== 'win32') {
      await fs.promises.chmod(binaryPath, 0o755);
   }

   const metadata: InstalledMetadata = {
      version: extensionVersion,
      archiveName: platformAsset.archiveName,
      binaryName: platformAsset.binaryName,
   };

   await fs.promises.writeFile(metadataPath, JSON.stringify(metadata, null, 2), 'utf8');
   await fs.promises.unlink(archivePath).catch(() => undefined);

   outputChannel.appendLine(`WA2: Installed wa2lsp ${extensionVersion}`);
}

async function extractArchive(args: {
   archivePath: string;
   archiveType: 'zip' | 'tar.gz';
   destinationDir: string;
   outputChannel: vscode.OutputChannel;
}): Promise<void> {
   const { archivePath, archiveType, destinationDir, outputChannel } = args;

   outputChannel.appendLine(`WA2: Extracting ${path.basename(archivePath)}`);

   if (archiveType === 'zip') {
      await extractZip(archivePath, { dir: destinationDir });
      return;
   }

   await tar.x({
      file: archivePath,
      cwd: destinationDir,
      gzip: true,
   });
}

async function readInstalledMetadata(metadataPath: string): Promise<InstalledMetadata | undefined> {
   try {
      const raw = await fs.promises.readFile(metadataPath, 'utf8');
      return JSON.parse(raw) as InstalledMetadata;
   } catch {
      return undefined;
   }
}

async function pathExists(filePath: string): Promise<boolean> {
   try {
      await fs.promises.access(filePath, fs.constants.F_OK);
      return true;
   } catch {
      return false;
   }
}

async function downloadFile(url: string, destinationPath: string): Promise<void> {
   await new Promise<void>((resolve, reject) => {
      const request = https.get(url, response => {
         if (
            response.statusCode &&
            response.statusCode >= 300 &&
            response.statusCode < 400 &&
            response.headers.location
         ) {
            response.resume();
            void downloadFile(response.headers.location, destinationPath).then(resolve, reject);
            return;
         }

         if (response.statusCode !== 200) {
            response.resume();
            reject(new Error(`HTTP ${response.statusCode ?? 'unknown'} downloading ${url}`));
            return;
         }

         const file = fs.createWriteStream(destinationPath);
         response.pipe(file);

         file.on('finish', () => {
            file.close();
            resolve();
         });

         file.on('error', err => {
            file.close();
            void fs.promises.unlink(destinationPath).catch(() => undefined);
            reject(err);
         });
      });

      request.on('error', err => {
         void fs.promises.unlink(destinationPath).catch(() => undefined);
         reject(err);
      });
   });
}

function formatError(err: unknown): string {
   if (err instanceof Error) {
      return err.message;
   }
   return String(err);
}