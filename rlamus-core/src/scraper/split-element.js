(element) => {
  if (!window.SPLIT_ELEMENT_BY_ID) {
    window.SPLIT_ELEMENT_BY_ID = new Map();
    window.SPLIT_ELEMENT_NEXT_ID = 0;
  }

  function rectEq(a, b) {
    return (
      a.width === b.width &&
      a.height === b.height &&
      a.left == b.left &&
      a.top == b.top
    );
  }

  function pushUnique(array, ...items) {
    for (const item of items) {
      if (!array.some((i) => rectEq(i, item))) {
        array.push(item);
      }
    }
  }

  function splitElement(element) {
    if (!element || !element.childNodes) {
      return [];
    }
    const parentRect = element.parentElement?.getBoundingClientRect?.() ?? {
      width: window.screen.width,
      height: window.screen.height,
    };
    const result = [];
    const sensibleNodes = [];
    for (const node of element.childNodes) {
      const boundingRect = node?.getBoundingClientRect?.();
      if (
        !boundingRect ||
        boundingRect.width <= 0 ||
        boundingRect.height <= 0
      ) {
        continue;
      }
      sensibleNodes.push([node, boundingRect]);
    }
    if (sensibleNodes.length === 1) {
      return splitElement(sensibleNodes[0][0]);
    }
    for (const [node, boundingRect] of sensibleNodes) {
      if (
        boundingRect.width >= parentRect.width &&
        boundingRect.height >= parentRect.height
      ) {
        return splitElement(node);
      }
      const id = window.SPLIT_ELEMENT_NEXT_ID++;
      window.SPLIT_ELEMENT_BY_ID.set(id, node);
      pushUnique(result, {
        width: boundingRect.width,
        height: boundingRect.height,
        left: boundingRect.left,
        top: boundingRect.top,
        id,
      });
    }
    return result;
  }

  return splitElement(
    typeof element === "number"
      ? window.SPLIT_ELEMENT_BY_ID.get(element)
      : element,
  );
};
