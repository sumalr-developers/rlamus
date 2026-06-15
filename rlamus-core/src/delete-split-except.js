(id) => {
  if (!window.SPLIT_ELEMENT_BY_ID) {
    throw new Error("Called before splitting");
  }
  const element = window.SPLIT_ELEMENT_BY_ID.get(id);
  if (!element) {
    throw new Error(`Element id ${id} not found`);
  }
  window.SPLIT_ELEMENT_BY_ID.forEach((ele, thisId) => {
    if (thisId !== id) {
      ele.remove();
    }
  });
  window.SPLIT_ELEMENT_BY_ID.clear();
};
