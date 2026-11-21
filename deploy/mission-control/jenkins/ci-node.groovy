import groovy.json.JsonOutput
import hudson.model.*
import hudson.slaves.*
import hudson.tasks.Shell
import jenkins.model.Jenkins

def j = Jenkins.get()
if (j.numExecutors != 0 || j.slaveAgentPort != -1 || !j.useSecurity) {
    throw new IllegalStateException('Unexpected controller boundary')
}
def name = 'forge-mission-control-ci'
def marker = 'Forge fixed exact-source verification v1'
def launcher = new JNLPLauncher()
launcher.setWebSocket(true)
def node = j.getNode(name)
if (node == null) {
    node = new DumbSlave(name, '/mnt/data/forge-homelab/jenkins-agent', launcher)
    node.setNodeDescription(marker)
    node.setNumExecutors(1)
    node.setMode(Node.Mode.EXCLUSIVE)
    node.setLabelString('forge-ci')
    j.addNode(node)
} else if (node.nodeDescription != marker || node.numExecutors != 1 || node.remoteFS != '/mnt/data/forge-homelab/jenkins-agent' || node.mode != Node.Mode.EXCLUSIVE) {
    throw new IllegalStateException('Existing CI node is not owned by this provisioner')
}
def project = j.getItem('forge-exact-candidate')
if (project != null && (!(project instanceof FreeStyleProject) || project.description != marker)) {
    throw new IllegalStateException('Refusing to replace an unrelated Jenkins job')
}
if (project == null) project = j.createProject(FreeStyleProject, 'forge-exact-candidate')
project.setDescription(marker)
project.setConcurrentBuild(false)
project.setAssignedLabel(j.getLabel('forge-ci'))
project.setDisabled(false)
project.addProperty(new ParametersDefinitionProperty([
    new StringParameterDefinition('FORGE_ACTION_ID', '', 'Durable verification action UUID'),
    new StringParameterDefinition('FORGE_WORKFLOW_ID', '', 'Workflow UUID'),
    new StringParameterDefinition('FORGE_CANDIDATE_COMMIT', '', 'Prepared exact candidate commit'),
    new StringParameterDefinition('FORGE_CANDIDATE_TREE', '', 'Prepared exact candidate tree'),
    new StringParameterDefinition('FORGE_EXPECTED_MAIN', '', 'Integration base'),
    new StringParameterDefinition('FORGE_GATE_MANIFEST', '', 'Fixed gate manifest SHA256'),
    new StringParameterDefinition('FORGE_CORRELATION_ID', '', 'Task correlation UUID')
]))
project.getBuildersList().replaceBy([new Shell('exec /usr/bin/sudo -n -- /usr/local/libexec/forge-ci-check "$FORGE_ACTION_ID"')])
project.save()
j.save()
println(JsonOutput.toJson([node:name, job:project.name, controller_executors:j.numExecutors, node_secret:node.toComputer().getJnlpMac(), online:!node.toComputer().offline]))
