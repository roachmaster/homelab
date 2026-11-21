import groovy.json.JsonOutput
import hudson.model.*
import hudson.tasks.Shell
import jenkins.model.Jenkins

def j = Jenkins.get()
if (j.numExecutors != 0 || j.slaveAgentPort != -1 || !j.useSecurity) {
    throw new IllegalStateException('Unexpected controller boundary')
}
def node = j.getNode('forge-mission-control-ci')
if (node == null || node.nodeDescription != 'Forge fixed exact-source verification v1' ||
    node.numExecutors != 1 || node.remoteFS != '/mnt/data/forge-homelab/jenkins-agent' ||
    node.mode != Node.Mode.EXCLUSIVE) {
    throw new IllegalStateException('The existing isolated CI executor is required')
}
def name = 'forge-native-candidate'
def marker = 'Forge pushed exact-source native verification v1'
def project = j.getItem(name)
if (project != null && (!(project instanceof FreeStyleProject) || project.description != marker)) {
    throw new IllegalStateException('Refusing to replace an unrelated Jenkins job')
}
if (project == null) project = j.createProject(FreeStyleProject, name)
project.setDescription(marker)
project.setConcurrentBuild(false)
project.setAssignedLabel(j.getLabel('forge-ci'))
project.setDisabled(false)
project.addProperty(new ParametersDefinitionProperty([
    new StringParameterDefinition('FORGE_ACTION_ID', '', 'Durable verification action UUID'),
    new StringParameterDefinition('FORGE_WORKFLOW_ID', '', 'Workflow UUID'),
    new StringParameterDefinition('FORGE_CANDIDATE_COMMIT', '', 'Pushed exact candidate commit'),
    new StringParameterDefinition('FORGE_CANDIDATE_TREE', '', 'Pushed exact candidate tree'),
    new StringParameterDefinition('FORGE_EXPECTED_MAIN', '', 'Observed main integration baseline'),
    new StringParameterDefinition('FORGE_GATE_MANIFEST', '', 'Fixed gate manifest SHA256'),
    new StringParameterDefinition('FORGE_CORRELATION_ID', '', 'Task correlation UUID')
]))
project.getBuildersList().replaceBy([new Shell('exec /usr/bin/sudo -n -- /usr/local/libexec/forge-native-ci-check "$FORGE_ACTION_ID"')])
project.save()
println(JsonOutput.toJson([node:node.nodeName, job:project.name, controller_executors:j.numExecutors,
    online:!node.toComputer().offline, no_job_submitted:true]))
