mod builder;
mod publisher;
mod subscription;
pub use self::builder::*;
pub use self::publisher::*;
pub use self::subscription::*;

use crate::rcl_bindings::*;
use crate::{Context, ParameterOverrideMap, QoSProfile, RclrsError, ToResult};

use std::cmp::PartialEq;
use std::ffi::CStr;
use std::fmt;
use std::sync::{Arc, Weak};
use std::vec::Vec;

use libc::c_char;
use parking_lot::Mutex;

use rosidl_runtime_rs::Message;

impl Drop for rcl_node_t {
    fn drop(&mut self) {
        // SAFETY: No preconditions for this function
        unsafe { rcl_node_fini(self).ok().unwrap() };
    }
}

// SAFETY: The functions accessing this type, including drop(), shouldn't care about the thread
// they are running in. Therefore, this type can be safely sent to another thread.
unsafe impl Send for rcl_node_t {}

/// A processing unit that can communicate with other nodes.
///
/// Nodes are a core concept in ROS 2. Refer to the official ["Understanding ROS 2 nodes"][1]
/// tutorial for an introduction.
///
/// Ownership of the node is shared with all [`Publisher`]s and [`Subscription`]s created from it.
/// That means that even after the node itself is dropped, it will continue to exist and be
/// displayed by e.g. `ros2 topic` as long as its publishers and subscriptions are not dropped.
///
/// # Naming
/// A node has a *name* and a *namespace*.
/// The node namespace will be prefixed to the node name to form the *fully qualified
/// node name*. This is the name that is shown e.g. in `ros2 node list`.
/// Similarly, the node namespace will be prefixed to all names of topics and services
/// created from this node.
///
/// By convention, a node name with a leading underscore marks the node as hidden.
///
/// It's a good idea for node names in the same executable to be unique.
///
/// ## Remapping
/// The namespace and name given when creating the node can be overriden through the command line.
/// In that sense, the parameters to the node creation functions are only the _default_ namespace and
/// name.
/// See also the [official tutorial][1] on the command line arguments for ROS nodes, and the
/// [`Node::namespace()`] and [`Node::name()`] functions for examples.
///
/// ## Rules for valid names
/// The rules for valid node names and node namespaces are explained in
/// [`NodeBuilder::new()`][3] and [`NodeBuilder::namespace()`][4].
///
/// [1]: https://docs.ros.org/en/rolling/Tutorials/Understanding-ROS2-Nodes.html
/// [2]: https://docs.ros.org/en/rolling/How-To-Guides/Node-arguments.html
/// [3]: crate::NodeBuilder::new
/// [4]: crate::NodeBuilder::namespace
pub struct Node {
    rcl_node_mtx: Arc<Mutex<rcl_node_t>>,
    pub(crate) rcl_context_mtx: Arc<Mutex<rcl_context_t>>,
    pub(crate) subscriptions: Vec<Weak<dyn SubscriptionBase>>,
    _parameter_map: ParameterOverrideMap,
}

impl Eq for Node {}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.rcl_node_mtx, &other.rcl_node_mtx)
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("Node")
            .field("fully_qualified_name", &self.fully_qualified_name())
            .finish()
    }
}

impl Node {
    /// Creates a new node in the empty namespace.
    ///
    /// See [`NodeBuilder::new()`] for documentation.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(context: &Context, node_name: &str) -> Result<Node, RclrsError> {
        Self::builder(context, node_name).build()
    }

    /// Returns the name of the node.
    ///
    /// This returns the name after remapping, so it is not necessarily the same as the name that
    /// was used when creating the node.
    ///
    /// # Example
    /// ```
    /// # use rclrs::{Context, RclrsError};
    /// // Without remapping
    /// let context = Context::new([])?;
    /// let node = context.create_node("my_node")?;
    /// assert_eq!(node.name(), "my_node");
    /// // With remapping
    /// let remapping = ["--ros-args", "-r", "__node:=your_node"].map(String::from);
    /// let context_r = Context::new(remapping)?;
    /// let node_r = context_r.create_node("my_node")?;
    /// assert_eq!(node_r.name(), "your_node");
    /// # Ok::<(), RclrsError>(())
    /// ```
    pub fn name(&self) -> String {
        self.call_string_getter(rcl_node_get_name)
    }

    /// Returns the namespace of the node.
    ///
    /// This returns the namespace after remapping, so it is not necessarily the same as the
    /// namespace that was used when creating the node.
    ///
    /// # Example
    /// ```
    /// # use rclrs::{Context, RclrsError};
    /// // Without remapping
    /// let context = Context::new([])?;
    /// let node = context
    ///   .create_node_builder("my_node")
    ///   .namespace("/my/namespace")
    ///   .build()?;
    /// assert_eq!(node.namespace(), "/my/namespace");
    /// // With remapping
    /// let remapping = ["--ros-args", "-r", "__ns:=/your_namespace"].map(String::from);
    /// let context_r = Context::new(remapping)?;
    /// let node_r = context_r.create_node("my_node")?;
    /// assert_eq!(node_r.namespace(), "/your_namespace");
    /// # Ok::<(), RclrsError>(())
    /// ```
    pub fn namespace(&self) -> String {
        self.call_string_getter(rcl_node_get_namespace)
    }

    /// Returns the fully qualified name of the node.
    ///
    /// The fully qualified name of the node is the node namespace combined with the node name.
    /// It is subject to the remappings shown in [`Node::name()`] and [`Node::namespace()`].
    ///
    /// # Example
    /// ```
    /// # use rclrs::{Context, RclrsError};
    /// let context = Context::new([])?;
    /// let node = context
    ///   .create_node_builder("my_node")
    ///   .namespace("/my/namespace")
    ///   .build()?;
    /// assert_eq!(node.fully_qualified_name(), "/my/namespace/my_node");
    /// # Ok::<(), RclrsError>(())
    /// ```
    pub fn fully_qualified_name(&self) -> String {
        self.call_string_getter(rcl_node_get_fully_qualified_name)
    }

    // Helper for name(), namespace(), fully_qualified_name()
    fn call_string_getter(
        &self,
        getter: unsafe extern "C" fn(*const rcl_node_t) -> *const c_char,
    ) -> String {
        unsafe { call_string_getter_with_handle(&*self.rcl_node_mtx.lock(), getter) }
    }

    /// Creates a [`Publisher`][1].
    ///
    /// [1]: crate::Publisher
    // TODO: make publisher's lifetime depend on node's lifetime
    pub fn create_publisher<T>(
        &self,
        topic: &str,
        qos: QoSProfile,
    ) -> Result<Publisher<T>, RclrsError>
    where
        T: Message,
    {
        Publisher::<T>::new(self, topic, qos)
    }

    /// Creates a [`Subscription`][1].
    ///
    /// [1]: crate::Subscription
    // TODO: make subscription's lifetime depend on node's lifetime
    pub fn create_subscription<T, F>(
        &mut self,
        topic: &str,
        qos: QoSProfile,
        callback: F,
    ) -> Result<Arc<Subscription<T>>, RclrsError>
    where
        T: Message,
        F: FnMut(T) + 'static + Send,
    {
        let subscription = Arc::new(Subscription::<T>::new(self, topic, qos, callback)?);
        self.subscriptions
            .push(Arc::downgrade(&subscription) as Weak<dyn SubscriptionBase>);
        Ok(subscription)
    }

    /// Returns the subscriptions that have not been dropped yet.
    pub(crate) fn live_subscriptions(&self) -> Vec<Arc<dyn SubscriptionBase>> {
        self.subscriptions
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// Returns the ROS domain ID that the node is using.
    ///    
    /// The domain ID controls which nodes can send messages to each other, see the [ROS 2 concept article][1].
    /// It can be set through the `ROS_DOMAIN_ID` environment variable.
    ///
    /// [1]: https://docs.ros.org/en/rolling/Concepts/About-Domain-ID.html
    ///
    /// # Example
    /// ```
    /// # use rclrs::{Context, RclrsError};
    /// // Set default ROS domain ID to 10 here
    /// std::env::set_var("ROS_DOMAIN_ID", "10");
    /// let context = Context::new([])?;
    /// let node = context.create_node("domain_id_node")?;
    /// let domain_id = node.domain_id();
    /// assert_eq!(domain_id, 10);
    /// # Ok::<(), RclrsError>(())
    /// ```
    // TODO: If node option is supported,
    // add description about this function is for getting actual domain_id
    // and about override of domain_id via node option
    pub fn domain_id(&self) -> usize {
        let rcl_node = &*self.rcl_node_mtx.lock();
        let mut domain_id: usize = 0;
        let ret = unsafe {
            // SAFETY: No preconditions for this function.
            rcl_node_get_domain_id(rcl_node, &mut domain_id)
        };

        debug_assert_eq!(ret, 0);
        domain_id
    }

    /// Creates a [`NodeBuilder`][1] with the given name.
    ///
    /// Convenience function equivalent to [`NodeBuilder::new()`][2].
    ///
    /// [1]: crate::NodeBuilder
    /// [2]: crate::NodeBuilder::new
    ///
    /// # Example
    /// ```
    /// # use rclrs::{Context, Node, RclrsError};
    /// let context = Context::new([])?;
    /// let node = Node::builder(&context, "my_node").build()?;
    /// assert_eq!(node.name(), "my_node");
    /// # Ok::<(), RclrsError>(())
    /// ```
    pub fn builder(context: &Context, node_name: &str) -> NodeBuilder {
        NodeBuilder::new(context, node_name)
    }
}

// Helper used to implement call_string_getter(), but also used to get the FQN in the Node::new()
// function, which is why it's not merged into Node::call_string_getter().
// This function is unsafe since it's possible to pass in an rcl_node_t with dangling
// pointers etc.
unsafe fn call_string_getter_with_handle(
    rcl_node: &rcl_node_t,
    getter: unsafe extern "C" fn(*const rcl_node_t) -> *const c_char,
) -> String {
    let char_ptr = getter(rcl_node);
    debug_assert!(!char_ptr.is_null());
    // SAFETY: The returned CStr is immediately converted to an owned string,
    // so the lifetime is no issue. The ptr is valid as per the documentation
    // of rcl_node_get_name.
    let cstr = CStr::from_ptr(char_ptr);
    cstr.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Node, QOS_PROFILE_DEFAULT};

    #[test]
    fn test_new() -> Result<(), RclrsError> {
        let context =
            Context::new(vec![]).expect("Context instantiation is expected to be a success");
        let _ = Node::new(&context, "Bob")?;

        Ok(())
    }

    #[test]
    fn test_create_subscription() -> Result<(), RclrsError> {
        let context =
            Context::new(vec![]).expect("Context instantiation is expected to be a success");
        let mut node = context.create_node("test_create_subscription")?;
        let _subscription = node.create_subscription::<std_msgs::msg::String, _>(
            "topic",
            QOS_PROFILE_DEFAULT,
            move |_: std_msgs::msg::String| {},
        )?;
        Ok(())
    }

    #[test]
    fn test_create_publisher() -> Result<(), RclrsError> {
        let context =
            Context::new(vec![]).expect("Context instantiation is expected to be a success");
        let node = context.create_node("test_create_publisher")?;
        let _publisher =
            node.create_publisher::<std_msgs::msg::String>("topic", QOS_PROFILE_DEFAULT)?;
        Ok(())
    }
}
