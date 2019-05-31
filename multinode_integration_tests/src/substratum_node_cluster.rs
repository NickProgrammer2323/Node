// Copyright (c) 2017-2019, Substratum LLC (https://substratum.net) and/or its affiliates. All rights reserved.
use crate::command::Command;
use crate::multinode_gossip::parse_gossip;
use crate::multinode_gossip::GossipType;
use crate::multinode_gossip::StandardBuilder;
use crate::substratum_mock_node::SubstratumMockNode;
use crate::substratum_node::SubstratumNode;
use crate::substratum_real_node::NodeStartupConfig;
use crate::substratum_real_node::NodeStartupConfigBuilder;
use crate::substratum_real_node::SubstratumRealNode;
use node_lib::sub_lib::cryptde::PublicKey;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::time::Duration;

pub struct SubstratumNodeCluster {
    real_nodes: HashMap<String, SubstratumRealNode>,
    mock_nodes: HashMap<String, SubstratumMockNode>,
    host_node_parent_dir: Option<String>,
    next_index: usize,
}

impl SubstratumNodeCluster {
    pub fn start() -> Result<SubstratumNodeCluster, String> {
        SubstratumNodeCluster::cleanup()?;
        SubstratumNodeCluster::create_network()?;
        let host_node_parent_dir = match env::var("HOST_NODE_PARENT_DIR") {
            Ok(ref hnpd) if !hnpd.is_empty() => Some(hnpd.clone()),
            _ => None,
        };
        if Self::is_in_jenkins() {
            SubstratumNodeCluster::interconnect_network()?;
        }
        Ok(SubstratumNodeCluster {
            real_nodes: HashMap::new(),
            mock_nodes: HashMap::new(),
            host_node_parent_dir,
            next_index: 1,
        })
    }

    pub fn next_index(&self) -> usize {
        self.next_index
    }

    pub fn start_real_node(&mut self, config: NodeStartupConfig) -> SubstratumRealNode {
        let index = self.next_index;
        self.next_index += 1;
        let node = SubstratumRealNode::start(config, index, self.host_node_parent_dir.clone());
        let name = node.name().to_string();
        self.real_nodes.insert(name.clone(), node);
        self.real_nodes.get(&name).unwrap().clone()
    }

    pub fn start_mock_node(&mut self, ports: Vec<u16>) -> SubstratumMockNode {
        let index = self.next_index;
        self.next_index += 1;
        let node = SubstratumMockNode::start(ports, index, self.host_node_parent_dir.clone());
        let name = node.name().to_string();
        self.mock_nodes.insert(name.clone(), node);
        self.mock_nodes.get(&name).unwrap().clone()
    }

    /// This method starts a linear neighborhood with node_count Nodes in it, all but two of which
    /// are fictional. It looks like this:
    ///
    ///   R === M === F === ... === F
    ///
    /// where R is a real Node, M is a mock Node, and the Fs are all fictional. The real Node's
    /// NeighborhoodDatabase will correspond to the diagram above. When it's finished,
    /// it returns a tuple containing the real Node and the mock Node.
    pub fn start_linear_neighborhood(
        &mut self,
        node_count: usize,
    ) -> (SubstratumRealNode, SubstratumMockNode) {
        let mock_node = self.start_mock_node(vec![10000]);
        let real_node = self.start_real_node(
            NodeStartupConfigBuilder::standard()
                .neighbor(mock_node.node_reference())
                .build(),
        );
        let (gossip, ip_addr) = mock_node.wait_for_gossip(Duration::from_secs(2)).unwrap();
        match parse_gossip(&gossip, ip_addr) {
            GossipType::DebutGossip(_) => (),
            _ => panic!(
                "Expected Debut gossip, but received {}",
                gossip.to_dot_graph(
                    ip_addr,
                    (mock_node.public_key(), &Some(mock_node.node_addr()))
                )
            ),
        }
        mock_node.transmit_debut(&real_node).unwrap();
        let standard_gossip = StandardBuilder::linear_neighborhood(
            &mock_node,
            real_node.public_key(),
            node_count - 1,
        )
        .build();
        mock_node
            .transmit_multinode_gossip(&real_node, &standard_gossip)
            .unwrap();
        (real_node, mock_node)
    }

    pub fn stop(self) {
        SubstratumNodeCluster::cleanup().unwrap()
    }

    pub fn stop_node(&mut self, name: &str) {
        match self.real_nodes.remove(name) {
            Some(node) => drop(node),
            None => match self.mock_nodes.remove(name) {
                Some(node) => drop(node),
                None => panic!("Node {} was not found in cluster", name),
            },
        };
    }

    pub fn running_node_names(&self) -> HashSet<String> {
        let mut node_name_refs = vec![];
        node_name_refs.extend(self.real_nodes.keys());
        node_name_refs.extend(self.mock_nodes.keys());
        node_name_refs.into_iter().map(|x| x.clone()).collect()
    }

    pub fn get_real_node_by_name(&self, name: &str) -> Option<SubstratumRealNode> {
        match self.real_nodes.get(name) {
            Some(node_ref) => Some(node_ref.clone()),
            None => None,
        }
    }

    pub fn get_real_node_by_key(&self, key: &PublicKey) -> Option<SubstratumRealNode> {
        match self
            .real_nodes
            .values()
            .into_iter()
            .find(|node| node.public_key() == key)
        {
            Some(node_ref) => Some(node_ref.clone()),
            None => None,
        }
    }

    pub fn get_mock_node_by_name(&self, name: &str) -> Option<SubstratumMockNode> {
        match self.mock_nodes.get(name) {
            Some(node_ref) => Some(node_ref.clone()),
            None => None,
        }
    }

    pub fn get_node_by_name(&self, name: &str) -> Option<Box<dyn SubstratumNode>> {
        match self.real_nodes.get(name) {
            Some(node_ref) => Some(Box::new(node_ref.clone())),
            None => match self.mock_nodes.get(name) {
                Some(node_ref) => Some(Box::new(node_ref.clone())),
                None => None,
            },
        }
    }

    pub fn is_in_jenkins() -> bool {
        match env::var("HOST_NODE_PARENT_DIR") {
            Ok(ref value) if value.is_empty() => false,
            Ok(_) => true,
            Err(_) => false,
        }
    }

    fn cleanup() -> Result<(), String> {
        SubstratumNodeCluster::stop_running_nodes()?;
        if Self::is_in_jenkins() {
            Self::disconnect_network()
        }
        SubstratumNodeCluster::remove_network_if_running()
    }

    fn stop_running_nodes() -> Result<(), String> {
        let mut command = Command::new(
            "docker",
            Command::strings(vec!["ps", "-q", "--filter", "ancestor=test_node_image"]),
        );
        if command.wait_for_exit() != 0 {
            return Err(format!(
                "Could not stop running nodes: {}",
                command.stderr_as_string()
            ));
        }
        let output = command.stdout_as_string();
        let results: Vec<String> = output
            .split("\n")
            .filter(|result| !result.is_empty())
            .map(|container_id| {
                let mut command = Command::new(
                    "docker",
                    Command::strings(vec!["stop", "-t", "0", container_id]),
                );
                match command.wait_for_exit() {
                    0 => Ok(()),
                    _ => Err(format!(
                        "Could not stop node '{}': {}",
                        container_id,
                        command.stderr_as_string()
                    )),
                }
            })
            .filter(|result| result.is_err())
            .map(|result| result.err().unwrap())
            .collect();
        if results.is_empty() {
            Ok(())
        } else {
            Err(results.join("; "))
        }
    }

    fn disconnect_network() {
        let mut command = Command::new(
            "docker",
            Command::strings(vec![
                "network",
                "disconnect",
                "integration_net",
                "subjenkins",
            ]),
        );
        command.wait_for_exit();
    }

    fn remove_network_if_running() -> Result<(), String> {
        let mut command = Command::new("docker", Command::strings(vec!["network", "ls"]));
        if command.wait_for_exit() != 0 {
            return Err(format!(
                "Could not list networks: {}",
                command.stderr_as_string()
            ));
        }
        let output = command.stdout_as_string();
        if !output.contains("integration_net") {
            return Ok(());
        }
        let mut command = Command::new(
            "docker",
            Command::strings(vec!["network", "rm", "integration_net"]),
        );
        match command.wait_for_exit() {
            0 => Ok(()),
            _ => Err(format!(
                "Could not remove network integration_net: {}",
                command.stderr_as_string()
            )),
        }
    }

    fn create_network() -> Result<(), String> {
        let mut command = Command::new(
            "docker",
            Command::strings(vec![
                "network",
                "create",
                "--subnet=172.18.0.0/16",
                "integration_net",
            ]),
        );
        match command.wait_for_exit() {
            0 => Ok(()),
            _ => Err(format!(
                "Could not create network integration_net: {}",
                command.stderr_as_string()
            )),
        }
    }

    fn interconnect_network() -> Result<(), String> {
        let mut command = Command::new(
            "docker",
            Command::strings(vec!["network", "connect", "integration_net", "subjenkins"]),
        );
        match command.wait_for_exit() {
            0 => Ok(()),
            _ => Err(format!(
                "Could not connect subjenkins to integration_net: {}",
                command.stderr_as_string()
            )),
        }
    }
}
