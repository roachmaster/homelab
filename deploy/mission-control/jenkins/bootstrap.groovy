import hudson.model.User
import hudson.security.HudsonPrivateSecurityRealm
import hudson.security.FullControlOnceLoggedInAuthorizationStrategy
import jenkins.install.InstallState
import jenkins.model.Jenkins
import jenkins.model.JenkinsLocationConfiguration

// Installed before the first start. Password comes from systemd credentials,
// never from a command argument, source file or log message.
def instance = Jenkins.get()
def marker = new File(instance.rootDir, '.forge-headless-bootstrap-v1')
if (!marker.exists()) {
    if (User.getById('forge-admin', false) != null) {
        throw new IllegalStateException('Unexpected existing administrator; refusing to replace security')
    }
    def credentialDirectory = System.getenv('CREDENTIALS_DIRECTORY')
    if (!credentialDirectory) {
        throw new IllegalStateException('Missing administrator credential directory')
    }
    def password = new File(credentialDirectory, 'adminPassword').getText('UTF-8').trim()
    if (password.length() < 32) {
        throw new IllegalStateException('Administrator credential is invalid')
    }
    def realm = new HudsonPrivateSecurityRealm(false, false, null)
    realm.createAccount('forge-admin', password)
    def authorization = new FullControlOnceLoggedInAuthorizationStrategy()
    authorization.setAllowAnonymousRead(false)
    instance.setSecurityRealm(realm)
    instance.setAuthorizationStrategy(authorization)
    instance.setNumExecutors(0)
    instance.setSlaveAgentPort(-1)
    def location = JenkinsLocationConfiguration.get()
    location.setUrl('http://127.0.0.1:18084/')
    location.save()
    instance.setInstallState(InstallState.INITIAL_SETUP_COMPLETED)
    instance.save()
    marker.setText('Forge headless controller v1; forge-admin; no build executors\n', 'UTF-8')
}
